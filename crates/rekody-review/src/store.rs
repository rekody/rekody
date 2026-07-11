//! Dataset state for the review tool: the capture manifest plus the two
//! review sidecars (teacher transcripts and human decisions).
//!
//! Files, all inside the training-data root:
//!
//!   manifest.jsonl            the raw capture manifest. Rewritten only for
//!                             accept_teacher/edit decisions, with the same
//!                             tmp + rename pattern rekody-core's
//!                             training_data::correct_text uses.
//!   manifest.jsonl.bak-review one-time backup taken before this tool's
//!                             first manifest write, ever.
//!   review.jsonl              append-only teacher transcripts, one line per
//!                             clip: audio_filepath, teacher_text,
//!                             teacher_model, wer, created.
//!   decisions.jsonl           append-only decision log, "undo" lines
//!                             included, replayed on startup to compute the
//!                             active decision per clip. Each decision line
//!                             carries the pre-decision manifest fields so
//!                             undo is lossless.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use serde_json::{Value, json};

pub type SharedStore = Arc<Mutex<Store>>;

/// Lock the shared store, riding through poison: the files on disk are
/// append-safe, so serving slightly stale state beats killing the server.
pub fn lock(store: &SharedStore) -> MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Dataset root: `$REKODY_TRAINING_DIR` or `~/.local/share/rekody/training-data`,
/// the same resolution rekody-core's training_data module uses.
pub fn dataset_dir() -> PathBuf {
    std::env::var("REKODY_TRAINING_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| {
                    h.join(".local")
                        .join("share")
                        .join("rekody")
                        .join("training-data")
                })
                .unwrap_or_else(|| PathBuf::from("training-data"))
        })
}

/// In-memory view of the dataset. Manifest lines are kept as raw JSON values
/// so unknown fields survive rewrites untouched.
pub struct Store {
    root: PathBuf,
    /// Manifest lines in file order.
    entries: Vec<Value>,
    /// audio_filepath -> index into `entries` (first occurrence wins).
    index: HashMap<String, usize>,
    /// audio_filepath -> teacher sidecar line.
    teacher: HashMap<String, Value>,
    /// audio_filepath -> active (not undone) decision line.
    decisions: HashMap<String, Value>,
}

impl Store {
    /// Load the manifest and replay both sidecars.
    pub fn load(root: PathBuf) -> Result<Self> {
        let manifest = root.join("manifest.jsonl");
        let contents = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;

        let mut entries = Vec::new();
        let mut index = HashMap::new();
        for (n, line) in contents.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(line)
                .with_context(|| format!("manifest line {} is not valid JSON", n + 1))?;
            let path = v["audio_filepath"].as_str().unwrap_or_default().to_string();
            if path.is_empty() {
                tracing::warn!(
                    line = n + 1,
                    "manifest line has no audio_filepath; skipping"
                );
                continue;
            }
            if index.contains_key(&path) {
                tracing::warn!(clip = %path, "duplicate manifest entry; keeping the first");
                continue;
            }
            index.insert(path, entries.len());
            entries.push(v);
        }

        let teacher = read_jsonl_map(&root.join("review.jsonl"), |_, v, map| {
            if let Some(p) = v["audio_filepath"].as_str() {
                map.insert(p.to_string(), v);
            }
        })?;
        // Decisions replay in order: an "undo" line cancels the clip's
        // current decision, anything else becomes the new active decision.
        let decisions = read_jsonl_map(&root.join("decisions.jsonl"), |_, v, map| {
            let Some(p) = v["audio_filepath"].as_str().map(String::from) else {
                return;
            };
            if v["decision"].as_str() == Some("undo") {
                map.remove(&p);
            } else {
                map.insert(p, v);
            }
        })?;

        Ok(Self {
            root,
            entries,
            index,
            teacher,
            decisions,
        })
    }

    pub fn clip_count(&self) -> usize {
        self.entries.len()
    }

    pub fn total_duration_secs(&self) -> f64 {
        self.entries
            .iter()
            .map(|e| e["duration"].as_f64().unwrap_or(0.0))
            .sum()
    }

    /// Clips already scored by the teacher (and still present in the manifest).
    pub fn teacher_count(&self) -> usize {
        self.index
            .keys()
            .filter(|p| self.teacher.contains_key(*p))
            .count()
    }

    pub fn decision_count(&self) -> usize {
        self.index
            .keys()
            .filter(|p| self.decisions.contains_key(*p))
            .count()
    }

    /// `(audio_filepath, label_text)` for every clip the teacher pass still
    /// needs to score, in manifest order. This is the resumability check:
    /// anything already in review.jsonl is skipped.
    pub fn pending_for_teacher(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter_map(|e| {
                let path = e["audio_filepath"].as_str()?;
                if self.teacher.contains_key(path) {
                    return None;
                }
                Some((
                    path.to_string(),
                    e["text"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .collect()
    }

    /// Append one teacher transcript to review.jsonl. A clip that somehow
    /// got scored twice keeps its first line.
    pub fn record_teacher(&mut self, path: &str, teacher_text: &str, wer: f64) -> Result<()> {
        if self.teacher.contains_key(path) {
            return Ok(());
        }
        let line = json!({
            "audio_filepath": path,
            "teacher_text": teacher_text,
            "teacher_model": crate::teacher::TEACHER_MODEL,
            "wer": wer,
            "created": iso_now(),
        });
        append_line(&self.root.join("review.jsonl"), &line)?;
        self.teacher.insert(path.to_string(), line);
        Ok(())
    }

    /// Apply one review decision and return the clip's updated JSON.
    ///
    /// accept_teacher and edit rewrite the manifest entry (text, corrected,
    /// label_source); keep_original only logs. A decision over an existing
    /// one first undoes it, so the log replays cleanly. "undo" reverts the
    /// manifest from the pre-decision fields stored in the decision line.
    pub fn apply_decision(
        &mut self,
        path: &str,
        decision: &str,
        final_text: Option<&str>,
    ) -> Result<Value> {
        let idx = *self
            .index
            .get(path)
            .with_context(|| format!("unknown clip {path}"))?;

        match decision {
            "undo" => self.undo(path)?,
            "accept_teacher" | "keep_original" | "edit" => {
                // Replace semantics: revert any prior decision first so
                // prev_* fields always describe the pre-review manifest.
                if self.decisions.contains_key(path) {
                    self.undo(path)?;
                }
                let resolved = match decision {
                    "accept_teacher" => self
                        .teacher
                        .get(path)
                        .and_then(|t| t["teacher_text"].as_str())
                        .context("teacher transcript is not ready for this clip yet")?
                        .to_string(),
                    "keep_original" => self.entries[idx]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    _ => {
                        let t = final_text.map(str::trim).unwrap_or_default();
                        anyhow::ensure!(!t.is_empty(), "edited text is empty");
                        t.to_string()
                    }
                };

                let entry = &self.entries[idx];
                let prev_text = entry["text"].as_str().unwrap_or_default().to_string();
                let prev_corrected = entry["corrected"].as_bool().unwrap_or(false);
                let prev_label_source = entry["label_source"].as_str().map(String::from);

                if decision != "keep_original" {
                    let source = if decision == "edit" {
                        "human"
                    } else {
                        "teacher"
                    };
                    let text = resolved.clone();
                    self.rewrite_manifest_entry(path, |v| {
                        v["text"] = Value::String(text);
                        v["corrected"] = Value::Bool(true);
                        v["label_source"] = Value::String(source.to_string());
                    })?;
                }

                let mut line = json!({
                    "audio_filepath": path,
                    "decision": decision,
                    "final_text": resolved,
                    "prev_text": prev_text,
                    "prev_corrected": prev_corrected,
                    "decided": iso_now(),
                });
                if let Some(src) = prev_label_source {
                    line["prev_label_source"] = Value::String(src);
                }
                append_line(&self.root.join("decisions.jsonl"), &line)?;
                self.decisions.insert(path.to_string(), line);
            }
            other => anyhow::bail!("unknown decision {other:?}"),
        }

        Ok(self.clip_json(idx))
    }

    /// Revert the clip's active decision: restore the pre-decision manifest
    /// fields (when the decision wrote the manifest) and log an undo line.
    fn undo(&mut self, path: &str) -> Result<()> {
        let record = self
            .decisions
            .get(path)
            .cloned()
            .with_context(|| format!("no decision to undo for {path}"))?;
        let wrote_manifest = matches!(
            record["decision"].as_str(),
            Some("accept_teacher") | Some("edit")
        );
        if wrote_manifest {
            let prev_text = record["prev_text"].as_str().unwrap_or_default().to_string();
            let prev_corrected = record["prev_corrected"].as_bool().unwrap_or(false);
            let prev_source = record["prev_label_source"].as_str().map(String::from);
            self.rewrite_manifest_entry(path, |v| {
                v["text"] = Value::String(prev_text);
                let obj = v.as_object_mut();
                // Restore the exact prior shape: save_pair never writes
                // corrected:false or label_source, so absent fields go back
                // to absent rather than false/null.
                if let Some(map) = obj {
                    if prev_corrected {
                        map.insert("corrected".into(), Value::Bool(true));
                    } else {
                        map.remove("corrected");
                    }
                    match prev_source {
                        Some(s) => {
                            map.insert("label_source".into(), Value::String(s));
                        }
                        None => {
                            map.remove("label_source");
                        }
                    }
                }
            })?;
        }
        append_line(
            &self.root.join("decisions.jsonl"),
            &json!({ "audio_filepath": path, "decision": "undo", "decided": iso_now() }),
        )?;
        self.decisions.remove(path);
        Ok(())
    }

    /// Rewrite one manifest entry atomically (tmp + rename), re-reading the
    /// file first so dictations the daemon appended since startup survive.
    /// Takes the one-time backup before the first write.
    fn rewrite_manifest_entry(
        &mut self,
        path: &str,
        mutate: impl FnOnce(&mut Value),
    ) -> Result<()> {
        let manifest = self.root.join("manifest.jsonl");
        let backup = self.root.join("manifest.jsonl.bak-review");
        if !backup.exists() {
            std::fs::copy(&manifest, &backup).context("writing manifest.jsonl.bak-review")?;
            tracing::info!(backup = %backup.display(), "one-time manifest backup taken");
        }

        let contents = std::fs::read_to_string(&manifest).context("re-reading manifest")?;
        let mut lines: Vec<String> = contents
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect();
        let pos = lines
            .iter()
            .position(|l| {
                serde_json::from_str::<Value>(l)
                    .ok()
                    .is_some_and(|v| v["audio_filepath"].as_str() == Some(path))
            })
            .with_context(|| format!("clip {path} vanished from the manifest"))?;
        let mut v: Value = serde_json::from_str(&lines[pos]).context("manifest line parse")?;
        mutate(&mut v);
        lines[pos] = v.to_string();
        if let Some(&i) = self.index.get(path) {
            self.entries[i] = v;
        }

        let tmp = manifest.with_extension("jsonl.tmp");
        std::fs::write(&tmp, lines.join("\n") + "\n").context("writing manifest tmp")?;
        std::fs::rename(&tmp, &manifest).context("replacing manifest")?;
        Ok(())
    }

    /// One clip's JSON for the API: manifest fields plus teacher and
    /// decision state.
    fn clip_json(&self, idx: usize) -> Value {
        let e = &self.entries[idx];
        let path = e["audio_filepath"].as_str().unwrap_or_default();
        let mut out = json!({
            "audio_filepath": path,
            "text": e["text"],
            "duration": e["duration"].as_f64().unwrap_or(0.0),
            "engine": e["engine"],
            "timestamp": e["timestamp"],
            "corrected": e["corrected"].as_bool().unwrap_or(false),
            "app_context": e.get("app_context").cloned().unwrap_or(Value::Null),
            "teacher_text": Value::Null,
            "wer": Value::Null,
            "decision": Value::Null,
        });
        if let Some(t) = self.teacher.get(path) {
            out["teacher_text"] = t["teacher_text"].clone();
            out["wer"] = t["wer"].clone();
        }
        if let Some(d) = self.decisions.get(path) {
            out["decision"] = json!({
                "decision": d["decision"],
                "final_text": d["final_text"],
                "decided": d["decided"],
            });
        }
        out
    }

    /// The `/api/clips` payload: every clip plus dataset-level counts.
    pub fn clips_json(&self) -> Value {
        let clips: Vec<Value> = (0..self.entries.len()).map(|i| self.clip_json(i)).collect();
        json!({
            "total": self.entries.len(),
            "transcribed": self.teacher_count(),
            "decided": self.decision_count(),
            "total_duration_secs": self.total_duration_secs(),
            "clips": clips,
        })
    }
}

/// Read a JSONL file line by line into a map via `fold`. Missing file means
/// an empty map (first run).
fn read_jsonl_map(
    path: &Path,
    fold: impl Fn(usize, Value, &mut HashMap<String, Value>),
) -> Result<HashMap<String, Value>> {
    let mut map = HashMap::new();
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(map),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    for (n, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => fold(n, v, &mut map),
            Err(e) => tracing::warn!(
                file = %path.display(),
                line = n + 1,
                "skipping corrupt sidecar line: {e}"
            ),
        }
    }
    Ok(map)
}

/// Append one JSON line atomically enough for a single-writer tool: the
/// whole line lands in one write on an O_APPEND handle, owner-only like the
/// manifest itself.
fn append_line(path: &Path, line: &Value) -> Result<()> {
    use std::io::Write;
    let mut buf = line.to_string();
    buf.push('\n');
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(buf.as_bytes())
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`, via the same civil_from_days
/// conversion training_data.rs and history.rs use (no chrono dependency).
pub fn iso_now() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two-clip manifest in a temp dir; returns the root.
    fn seed_dataset() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            json!({
                "audio_filepath": "audio/a.flac",
                "text": "the spark gdx",
                "duration": 2.5,
                "engine": "nemotron",
                "timestamp": "2026-07-01T10-00-00",
            }),
            json!({
                "audio_filepath": "audio/b.flac",
                "text": "second utterance",
                "duration": 3.0,
                "engine": "nemotron",
                "timestamp": "2026-07-01T10-01-00",
                "app_context": "Ghostty",
            }),
        ];
        let body = lines.map(|v| v.to_string()).join("\n") + "\n";
        std::fs::write(dir.path().join("manifest.jsonl"), body).unwrap();
        dir
    }

    fn manifest_entry(root: &Path, path: &str) -> Value {
        let contents = std::fs::read_to_string(root.join("manifest.jsonl")).unwrap();
        contents
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v["audio_filepath"] == path)
            .unwrap()
    }

    #[test]
    fn accept_teacher_rewrites_manifest_and_undo_reverts_losslessly() {
        let dir = seed_dataset();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .record_teacher("audio/a.flac", "the Spark DGX", 0.4)
            .unwrap();

        store
            .apply_decision("audio/a.flac", "accept_teacher", None)
            .unwrap();
        let entry = manifest_entry(dir.path(), "audio/a.flac");
        assert_eq!(entry["text"], "the Spark DGX");
        assert_eq!(entry["corrected"], true);
        assert_eq!(entry["label_source"], "teacher");
        // One-time backup exists and still holds the original label.
        let bak = std::fs::read_to_string(dir.path().join("manifest.jsonl.bak-review")).unwrap();
        assert!(bak.contains("the spark gdx"));
        // Neighbor untouched.
        assert_eq!(
            manifest_entry(dir.path(), "audio/b.flac")["text"],
            "second utterance"
        );

        store.apply_decision("audio/a.flac", "undo", None).unwrap();
        let entry = manifest_entry(dir.path(), "audio/a.flac");
        assert_eq!(entry["text"], "the spark gdx");
        // Pristine shape restored: no corrected/label_source keys at all.
        assert!(entry.get("corrected").is_none());
        assert!(entry.get("label_source").is_none());
        assert_eq!(store.decision_count(), 0);
    }

    #[test]
    fn keep_original_logs_without_touching_the_manifest() {
        let dir = seed_dataset();
        let before = std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .apply_decision("audio/b.flac", "keep_original", None)
            .unwrap();
        let after = std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap();
        assert_eq!(before, after, "keep_original must not rewrite the manifest");
        assert!(!dir.path().join("manifest.jsonl.bak-review").exists());

        // The decision log replays on a fresh load.
        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.decision_count(), 1);
        let clip = &reloaded.clips_json()["clips"][1];
        assert_eq!(clip["decision"]["decision"], "keep_original");
        assert_eq!(clip["decision"]["final_text"], "second utterance");
    }

    #[test]
    fn later_decision_replaces_earlier_and_replays_cleanly() {
        let dir = seed_dataset();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .record_teacher("audio/a.flac", "the Spark DGX", 0.4)
            .unwrap();
        store
            .apply_decision("audio/a.flac", "accept_teacher", None)
            .unwrap();
        store
            .apply_decision("audio/a.flac", "edit", Some("the Spark DGX box"))
            .unwrap();

        let entry = manifest_entry(dir.path(), "audio/a.flac");
        assert_eq!(entry["text"], "the Spark DGX box");
        assert_eq!(entry["label_source"], "human");
        // prev_* on the active decision still points at the ORIGINAL label,
        // because the replace path undoes the first decision before editing.
        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.decision_count(), 1);
        let clip = &reloaded.clips_json()["clips"][0];
        assert_eq!(clip["decision"]["decision"], "edit");

        // And undoing the edit lands back on the original label.
        let mut reloaded = reloaded;
        reloaded
            .apply_decision("audio/a.flac", "undo", None)
            .unwrap();
        assert_eq!(
            manifest_entry(dir.path(), "audio/a.flac")["text"],
            "the spark gdx"
        );
    }

    #[test]
    fn teacher_sidecar_resumes_and_survives_reload() {
        let dir = seed_dataset();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.pending_for_teacher().len(), 2);
        store
            .record_teacher("audio/a.flac", "the Spark DGX", 0.4)
            .unwrap();
        assert_eq!(store.pending_for_teacher().len(), 1);

        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.teacher_count(), 1);
        assert_eq!(
            reloaded.pending_for_teacher(),
            vec![("audio/b.flac".to_string(), "second utterance".to_string())]
        );
    }
}
