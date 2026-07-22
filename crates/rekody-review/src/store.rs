//! Dataset state for the review tool: the capture manifest plus the review
//! sidecars (teacher transcripts, human decisions, session state).
//!
//! Files, all inside the training-data root:
//!
//!   manifest.jsonl            the raw capture manifest. Rewritten only for
//!                             accept_teacher/edit/delete decisions, with
//!                             the same tmp + rename pattern rekody-core's
//!                             training_data::correct_text uses.
//!   manifest.jsonl.bak-review one-time backup taken before this tool's
//!                             first manifest write, ever.
//!   review.jsonl              append-only teacher transcripts, one line per
//!                             clip: audio_filepath, teacher_text,
//!                             teacher_model, wer, created. An originals-only
//!                             session seeds placeholder rows with null
//!                             teacher_text/wer; a real score appended later
//!                             replaces the placeholder (last line wins on
//!                             replay).
//!   decisions.jsonl           append-only decision log, "undo" lines
//!                             included, replayed on startup to compute the
//!                             active decision per clip. Each decision line
//!                             carries the pre-decision manifest fields so
//!                             undo is lossless. "delete" lines additionally
//!                             carry duration/app_context so the done list
//!                             can still describe the clip; deletes are the
//!                             one decision undo refuses (the audio file is
//!                             gone).
//!   review-state.json         how the session was started: second_opinion
//!                             plus started_at. Absent while review.jsonl is
//!                             also absent = the first-run screen.

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

/// Session state sidecar filename (inside the training-data root).
pub const REVIEW_STATE_FILE: &str = "review-state.json";

/// Which screen the page opens on, derived from the sidecars on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Nothing recorded yet: the page shows the first-run choice.
    FirstRun,
    /// A session is underway, with or without the second opinion.
    Session { second_opinion: bool },
}

/// Read review-state.json, `None` when absent or unreadable.
pub fn read_review_state(root: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(root.join(REVIEW_STATE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write review-state.json (tmp + rename). An existing started_at is kept:
/// upgrading an originals-only session to a second opinion does not restart
/// the session clock.
pub fn write_review_state(root: &Path, second_opinion: bool) -> Result<()> {
    let started_at = read_review_state(root)
        .and_then(|v| v["started_at"].as_str().map(String::from))
        .unwrap_or_else(iso_now);
    let v = json!({ "second_opinion": second_opinion, "started_at": started_at });
    let path = root.join(REVIEW_STATE_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, v.to_string()).context("writing review-state tmp")?;
    std::fs::rename(&tmp, &path).context("replacing review-state.json")?;
    Ok(())
}

/// In-memory view of the dataset. Manifest lines are kept as raw JSON values
/// so unknown fields survive rewrites untouched.
pub struct Store {
    root: PathBuf,
    /// Whether manifest.jsonl existed at load time (false = the first-run
    /// screen shows its "not found" state; the server still runs).
    manifest_found: bool,
    /// Manifest lines in file order.
    entries: Vec<Value>,
    /// audio_filepath -> index into `entries` (first occurrence wins).
    index: HashMap<String, usize>,
    /// audio_filepath -> teacher sidecar line (placeholders included).
    teacher: HashMap<String, Value>,
    /// audio_filepath -> active (not undone) decision line.
    decisions: HashMap<String, Value>,
}

impl Store {
    /// Load the manifest and replay both sidecars. A missing manifest is an
    /// empty dataset, not an error, so the first-run screen can say so.
    pub fn load(root: PathBuf) -> Result<Self> {
        let manifest = root.join("manifest.jsonl");
        let contents = match std::fs::read_to_string(&manifest) {
            Ok(c) => Some(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", manifest.display()));
            }
        };
        let manifest_found = contents.is_some();

        let mut entries = Vec::new();
        let mut index = HashMap::new();
        for (n, line) in contents.unwrap_or_default().lines().enumerate() {
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

        let mut store = Self {
            root,
            manifest_found,
            entries,
            index,
            teacher,
            decisions,
        };
        store.heal_interrupted_deletes()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest_found(&self) -> bool {
        self.manifest_found
    }

    /// Which screen the page opens on. review-state.json is authoritative;
    /// a dataset with teacher scores but no state file (from before the
    /// state file existed) is a running second-opinion session.
    pub fn effective_mode(&self) -> Mode {
        if let Some(state) = read_review_state(&self.root) {
            return Mode::Session {
                second_opinion: state["second_opinion"].as_bool().unwrap_or(false),
            };
        }
        if self.root.join("review.jsonl").exists() {
            return Mode::Session {
                second_opinion: true,
            };
        }
        Mode::FirstRun
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

    /// Clips actually scored by the teacher (still present in the manifest;
    /// originals-only placeholder rows do not count).
    pub fn teacher_count(&self) -> usize {
        self.index
            .keys()
            .filter(|p| {
                self.teacher
                    .get(*p)
                    .is_some_and(|t| !t["teacher_text"].is_null())
            })
            .count()
    }

    /// Live clips with an active decision.
    pub fn decision_count(&self) -> usize {
        self.index
            .keys()
            .filter(|p| self.decisions.contains_key(*p))
            .count()
    }

    /// Clips removed by a delete decision (no longer in the manifest).
    pub fn deleted_count(&self) -> usize {
        self.deleted_paths().len()
    }

    /// Every decided clip, deletes included: the "reviewed" number.
    pub fn reviewed_count(&self) -> usize {
        self.decision_count() + self.deleted_count()
    }

    fn deleted_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .decisions
            .iter()
            .filter(|(p, d)| d["decision"] == "delete" && !self.index.contains_key(*p))
            .map(|(p, _)| p.clone())
            .collect();
        paths.sort();
        paths
    }

    /// `(audio_filepath, label_text)` for every clip the teacher pass still
    /// needs to score, in manifest order. This is the resumability check:
    /// anything with a real transcript in review.jsonl is skipped, while
    /// originals-only placeholder rows stay pending so "add a second
    /// opinion" can score them later.
    pub fn pending_for_teacher(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter_map(|e| {
                let path = e["audio_filepath"].as_str()?;
                if self
                    .teacher
                    .get(path)
                    .is_some_and(|t| !t["teacher_text"].is_null())
                {
                    return None;
                }
                Some((
                    path.to_string(),
                    e["text"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .collect()
    }

    /// Append one teacher transcript to review.jsonl. A clip that already
    /// has a real transcript keeps its first line; a placeholder row from an
    /// originals-only start is replaced, and the replay rule (last line
    /// wins) agrees with that on reload.
    pub fn record_teacher(&mut self, path: &str, teacher_text: &str, wer: f64) -> Result<()> {
        if self
            .teacher
            .get(path)
            .is_some_and(|t| !t["teacher_text"].is_null())
        {
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

    /// Originals-only start: give every clip a review.jsonl row with no
    /// suggestion (null teacher_text/wer). The page then reviews the
    /// manifest labels as-is; a later upgrade scores exactly these rows.
    /// Returns how many rows were seeded (already-present rows are kept).
    pub fn seed_null_reviews(&mut self) -> Result<usize> {
        let missing: Vec<String> = self
            .entries
            .iter()
            .filter_map(|e| {
                let p = e["audio_filepath"].as_str()?;
                (!self.teacher.contains_key(p)).then(|| p.to_string())
            })
            .collect();
        for path in &missing {
            let line = json!({
                "audio_filepath": path,
                "teacher_text": Value::Null,
                "teacher_model": Value::Null,
                "wer": Value::Null,
                "created": iso_now(),
            });
            append_line(&self.root.join("review.jsonl"), &line)?;
            self.teacher.insert(path.clone(), line);
        }
        Ok(missing.len())
    }

    /// Apply one review decision and return the clip's updated JSON.
    ///
    /// accept_teacher and edit rewrite the manifest entry (text, corrected,
    /// label_source); keep_original only logs; delete drops the manifest row
    /// and the audio file. A decision over an existing one first undoes it,
    /// so the log replays cleanly. "undo" reverts the manifest from the
    /// pre-decision fields stored in the decision line, except for deletes,
    /// which cannot be undone (the file is gone).
    pub fn apply_decision(
        &mut self,
        path: &str,
        decision: &str,
        final_text: Option<&str>,
    ) -> Result<Value> {
        // Deletes run before the index lookup so a repeated delete for a
        // clip that is already gone stays a calm no-op.
        if decision == "delete" {
            return self.delete_clip(path);
        }
        // Any other action against a deleted clip gets the real reason,
        // not a generic "unknown clip".
        if !self.index.contains_key(path)
            && self
                .decisions
                .get(path)
                .is_some_and(|d| d["decision"] == "delete")
        {
            anyhow::bail!("that clip was deleted; its audio file is gone and cannot be restored");
        }

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
                        .context("the second opinion is not ready for this clip yet")?
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

    /// Delete a clip: log the decision, drop the manifest row (atomic
    /// rewrite, same one-time backup), and remove the audio file from disk.
    /// Deliberate product behavior: not recoverable, so the page confirms
    /// first and the done list offers no undo. Ordering matters: the
    /// decision line lands first, so a crash mid-delete is finished by
    /// `heal_interrupted_deletes` on the next load.
    fn delete_clip(&mut self, path: &str) -> Result<Value> {
        if !self.index.contains_key(path) {
            if self
                .decisions
                .get(path)
                .is_some_and(|d| d["decision"] == "delete")
            {
                return Ok(self.deleted_clip_json(path));
            }
            anyhow::bail!("unknown clip {path}");
        }
        // Replace semantics like every other decision: a prior decision is
        // undone first so prev_* describes the pre-review manifest.
        if self.decisions.contains_key(path) {
            self.undo(path)?;
        }
        let entry = &self.entries[self.index[path]];
        let mut line = json!({
            "audio_filepath": path,
            "decision": "delete",
            "prev_text": entry["text"].as_str().unwrap_or_default(),
            "prev_corrected": entry["corrected"].as_bool().unwrap_or(false),
            "duration": entry["duration"].as_f64().unwrap_or(0.0),
            "decided": iso_now(),
        });
        if let Some(src) = entry["label_source"].as_str() {
            line["prev_label_source"] = Value::String(src.to_string());
        }
        if let Some(app) = entry.get("app_context").and_then(Value::as_str) {
            line["app_context"] = Value::String(app.to_string());
        }
        append_line(&self.root.join("decisions.jsonl"), &line)?;
        self.decisions.insert(path.to_string(), line);
        self.remove_manifest_entry(path)?;
        self.remove_audio_file(path);
        Ok(self.deleted_clip_json(path))
    }

    /// Finish deletes that were logged but not applied (a crash between the
    /// decision append and the manifest rewrite). Idempotent.
    fn heal_interrupted_deletes(&mut self) -> Result<()> {
        let stranded: Vec<String> = self
            .index
            .keys()
            .filter(|p| {
                self.decisions
                    .get(*p)
                    .is_some_and(|d| d["decision"] == "delete")
            })
            .cloned()
            .collect();
        for path in stranded {
            tracing::warn!(clip = %path, "finishing an interrupted delete from a previous run");
            self.remove_manifest_entry(&path)?;
            self.remove_audio_file(&path);
        }
        Ok(())
    }

    /// Remove the clip's audio from disk; a missing file is already the goal.
    fn remove_audio_file(&self, path: &str) {
        let abs = self.root.join(path);
        match std::fs::remove_file(&abs) {
            Ok(()) => tracing::info!(clip = %path, "audio file deleted"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(clip = %path, "could not delete the audio file: {e}"),
        }
    }

    /// Revert the clip's active decision: restore the pre-decision manifest
    /// fields (when the decision wrote the manifest) and log an undo line.
    /// Deletes are refused: the audio file is gone.
    fn undo(&mut self, path: &str) -> Result<()> {
        let record = self
            .decisions
            .get(path)
            .cloned()
            .with_context(|| format!("no decision to undo for {path}"))?;
        anyhow::ensure!(
            record["decision"] != "delete",
            "deleted clips cannot be restored; the audio file is gone"
        );
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

    /// Rewrite manifest.jsonl atomically (tmp + rename) via `mutate` over
    /// its non-empty lines, re-reading the file first so dictations the
    /// daemon appended since startup survive. Takes the one-time backup
    /// before this tool's first write, ever.
    fn rewrite_manifest_lines(
        &mut self,
        mutate: impl FnOnce(&mut Vec<String>) -> Result<()>,
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
        mutate(&mut lines)?;

        let body = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        };
        let tmp = manifest.with_extension("jsonl.tmp");
        std::fs::write(&tmp, body).context("writing manifest tmp")?;
        std::fs::rename(&tmp, &manifest).context("replacing manifest")?;
        Ok(())
    }

    /// Rewrite one manifest entry in place.
    fn rewrite_manifest_entry(
        &mut self,
        path: &str,
        mutate: impl FnOnce(&mut Value),
    ) -> Result<()> {
        let mut updated: Option<Value> = None;
        self.rewrite_manifest_lines(|lines| {
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
            updated = Some(v);
            Ok(())
        })?;
        if let (Some(v), Some(&i)) = (updated, self.index.get(path)) {
            self.entries[i] = v;
        }
        Ok(())
    }

    /// Drop one manifest entry entirely (the delete path).
    fn remove_manifest_entry(&mut self, path: &str) -> Result<()> {
        self.rewrite_manifest_lines(|lines| {
            let before = lines.len();
            lines.retain(|l| {
                serde_json::from_str::<Value>(l)
                    .ok()
                    .is_none_or(|v| v["audio_filepath"].as_str() != Some(path))
            });
            anyhow::ensure!(
                lines.len() < before,
                "clip {path} vanished from the manifest"
            );
            Ok(())
        })?;
        if let Some(i) = self.index.remove(path) {
            self.entries.remove(i);
            self.index = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(n, e)| e["audio_filepath"].as_str().map(|p| (p.to_string(), n)))
                .collect();
        }
        Ok(())
    }

    /// One live clip's JSON for the API: manifest fields plus teacher and
    /// decision state. `prev_text` rides along on decisions so the page can
    /// count corrected words without another endpoint.
    fn clip_json(&self, idx: usize) -> Value {
        let e = &self.entries[idx];
        let path = e["audio_filepath"].as_str().unwrap_or_default();
        let mut out = json!({
            "audio_filepath": path,
            "deleted": false,
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
                "prev_text": d["prev_text"],
            });
        }
        out
    }

    /// A deleted clip's JSON: enough for the done list (source, duration,
    /// what the label said) plus the decision record.
    fn deleted_clip_json(&self, path: &str) -> Value {
        let d = self
            .decisions
            .get(path)
            .cloned()
            .unwrap_or_else(|| json!({}));
        json!({
            "audio_filepath": path,
            "deleted": true,
            "text": d["prev_text"].clone(),
            "duration": d["duration"].as_f64().unwrap_or(0.0),
            "app_context": d.get("app_context").cloned().unwrap_or(Value::Null),
            "corrected": false,
            "teacher_text": Value::Null,
            "wer": Value::Null,
            "decision": {
                "decision": "delete",
                "final_text": Value::Null,
                "decided": d["decided"].clone(),
                "prev_text": d["prev_text"].clone(),
            },
        })
    }

    /// The `/api/clips` payload: every clip (deleted ones included, for the
    /// done list) plus dataset-level counts.
    pub fn clips_json(&self) -> Value {
        let mut clips: Vec<Value> = (0..self.entries.len()).map(|i| self.clip_json(i)).collect();
        for path in self.deleted_paths() {
            clips.push(self.deleted_clip_json(&path));
        }
        json!({
            "total": self.clip_count() + self.deleted_count(),
            "transcribed": self.teacher_count(),
            "decided": self.reviewed_count(),
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

    /// Three clips with real (tiny) audio files on disk, for delete tests.
    fn seed_three_with_audio() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        let mut lines = Vec::new();
        for (name, text) in [
            ("a", "alpha one"),
            ("b", "bravo two"),
            ("c", "charlie three"),
        ] {
            lines.push(
                json!({
                    "audio_filepath": format!("audio/{name}.flac"),
                    "text": text,
                    "duration": 2.0,
                    "engine": "nemotron",
                    "timestamp": "2026-07-01T10-00-00",
                    "app_context": "Ghostty",
                })
                .to_string(),
            );
            std::fs::write(audio.join(format!("{name}.flac")), b"fLaC").unwrap();
        }
        std::fs::write(dir.path().join("manifest.jsonl"), lines.join("\n") + "\n").unwrap();
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

    #[test]
    fn delete_removes_row_and_audio_and_repeats_are_noops() {
        let dir = seed_three_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();

        let out = store
            .apply_decision("audio/b.flac", "delete", None)
            .unwrap();
        assert_eq!(out["deleted"], true);
        assert_eq!(out["decision"]["decision"], "delete");
        assert!(!dir.path().join("audio").join("b.flac").exists());
        let manifest = std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap();
        assert!(!manifest.contains("audio/b.flac"));
        assert!(manifest.contains("audio/a.flac") && manifest.contains("audio/c.flac"));
        assert!(dir.path().join("manifest.jsonl.bak-review").exists());

        // Repeat delete: calm no-op, still reported deleted.
        let again = store
            .apply_decision("audio/b.flac", "delete", None)
            .unwrap();
        assert_eq!(again["deleted"], true);

        // Undo is refused: the audio file is gone.
        assert!(store.apply_decision("audio/b.flac", "undo", None).is_err());

        assert_eq!(store.clip_count(), 2);
        assert_eq!(store.deleted_count(), 1);
        assert_eq!(store.reviewed_count(), 1);
    }

    #[test]
    fn mixed_decisions_and_delete_replay_together() {
        let dir = seed_three_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .record_teacher("audio/a.flac", "alpha won", 0.5)
            .unwrap();
        store
            .apply_decision("audio/a.flac", "accept_teacher", None)
            .unwrap();
        store
            .apply_decision("audio/b.flac", "delete", None)
            .unwrap();
        store
            .apply_decision("audio/c.flac", "edit", Some("charlie tree"))
            .unwrap();

        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.clip_count(), 2);
        assert_eq!(reloaded.deleted_count(), 1);
        assert_eq!(reloaded.reviewed_count(), 3);
        let clips = reloaded.clips_json();
        assert_eq!(clips["total"], 3);
        assert_eq!(clips["decided"], 3);

        // The deleted clip still shows up for the done list, marked deleted,
        // and carries what the label said plus the decision record.
        let rows = clips["clips"].as_array().unwrap();
        let deleted: Vec<&Value> = rows.iter().filter(|c| c["deleted"] == true).collect();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0]["audio_filepath"], "audio/b.flac");
        assert_eq!(deleted[0]["text"], "bravo two");
        assert_eq!(deleted[0]["decision"]["decision"], "delete");

        // Survivors carry their decisions with prev_text for the page.
        assert_eq!(
            manifest_entry(dir.path(), "audio/a.flac")["text"],
            "alpha won"
        );
        assert_eq!(
            manifest_entry(dir.path(), "audio/c.flac")["text"],
            "charlie tree"
        );
        let a = rows
            .iter()
            .find(|c| c["audio_filepath"] == "audio/a.flac")
            .unwrap();
        assert_eq!(a["decision"]["prev_text"], "alpha one");

        // Undo of a surviving decision still replays losslessly next to a
        // delete in the same log.
        let mut reloaded = reloaded;
        reloaded
            .apply_decision("audio/a.flac", "undo", None)
            .unwrap();
        assert_eq!(
            manifest_entry(dir.path(), "audio/a.flac")["text"],
            "alpha one"
        );
        let replayed = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(replayed.reviewed_count(), 2);
        assert_eq!(replayed.deleted_count(), 1);
    }

    #[test]
    fn interrupted_delete_is_healed_on_load() {
        let dir = seed_three_with_audio();
        // Simulate a crash after the decision append, before the manifest
        // rewrite: log the delete by hand, leave manifest + file in place.
        append_line(
            &dir.path().join("decisions.jsonl"),
            &json!({
                "audio_filepath": "audio/c.flac",
                "decision": "delete",
                "prev_text": "charlie three",
                "prev_corrected": false,
                "duration": 2.0,
                "decided": "2026-07-01T10:00:00Z",
            }),
        )
        .unwrap();

        let store = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.clip_count(), 2);
        assert_eq!(store.deleted_count(), 1);
        let manifest = std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap();
        assert!(!manifest.contains("audio/c.flac"));
        assert!(!dir.path().join("audio").join("c.flac").exists());
    }

    #[test]
    fn first_run_transitions_to_both_session_modes() {
        let dir = seed_dataset();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.effective_mode(), Mode::FirstRun);

        // Originals-only start: state file plus placeholder rows.
        write_review_state(dir.path(), false).unwrap();
        assert_eq!(store.seed_null_reviews().unwrap(), 2);
        assert_eq!(
            store.effective_mode(),
            Mode::Session {
                second_opinion: false
            }
        );
        assert_eq!(store.teacher_count(), 0, "placeholder rows are not scores");
        assert_eq!(
            store.pending_for_teacher().len(),
            2,
            "placeholder rows stay pending for a later upgrade"
        );
        let clip = store.clips_json()["clips"][0].clone();
        assert!(clip["teacher_text"].is_null() && clip["wer"].is_null());

        // Upgrading to the second opinion keeps the original start time.
        let started = read_review_state(dir.path()).unwrap()["started_at"].clone();
        write_review_state(dir.path(), true).unwrap();
        assert_eq!(
            store.effective_mode(),
            Mode::Session {
                second_opinion: true
            }
        );
        assert_eq!(
            read_review_state(dir.path()).unwrap()["started_at"],
            started
        );

        // A real score replaces the placeholder, on reload too (last line
        // wins in the replay).
        store
            .record_teacher("audio/a.flac", "the Spark DGX", 0.4)
            .unwrap();
        assert_eq!(store.teacher_count(), 1);
        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.teacher_count(), 1);
        assert_eq!(reloaded.pending_for_teacher().len(), 1);
    }

    #[test]
    fn legacy_dataset_with_scores_is_a_second_opinion_session() {
        let dir = seed_dataset();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.effective_mode(), Mode::FirstRun);
        store
            .record_teacher("audio/a.flac", "the Spark DGX", 0.4)
            .unwrap();
        // review.jsonl now exists without review-state.json: that is the
        // pre-product layout, treated as a scored session in progress.
        assert_eq!(
            store.effective_mode(),
            Mode::Session {
                second_opinion: true
            }
        );
    }

    #[test]
    fn missing_manifest_loads_as_an_empty_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::load(dir.path().to_path_buf()).unwrap();
        assert!(!store.manifest_found());
        assert_eq!(store.clip_count(), 0);
        assert_eq!(store.effective_mode(), Mode::FirstRun);
    }
}
