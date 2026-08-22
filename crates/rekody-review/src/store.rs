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
//!   exports/cut-…/            training-ready export cuts (reviewed clips
//!                             only), written by `export_cut`. Everything
//!                             else stays untouched: export is read-only
//!                             over the dataset.

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
    /// True while an outer operation (an import) already holds the dataset
    /// lock, so the per-rewrite acquisition inside `rewrite_manifest_lines`
    /// must not try to take it a second time and deadlock against itself.
    lock_held: bool,
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
            lock_held: false,
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

    /// Seconds of audio the user has actually reviewed: clips still in the
    /// manifest carrying an effective decision (keep_original, edit,
    /// accept_teacher). Deleted clips are excluded, their audio is gone.
    /// This is the number the page shows as hours reviewed.
    pub fn reviewed_duration_secs(&self) -> f64 {
        self.entries
            .iter()
            .filter(|e| {
                e["audio_filepath"]
                    .as_str()
                    .and_then(|p| self.decisions.get(p))
                    .is_some_and(is_effective_decision)
            })
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
        self.apply_decision_inner(path, decision, final_text, None)
    }

    /// The body of [`Store::apply_decision`], with the extra hook an import
    /// needs: `merge` supplies the label another machine settled on, keeps
    /// that machine's `decided` timestamp verbatim (newest-wins compares
    /// those on the next import), and stamps `merged_from` on the line so
    /// the origin stays auditable and undo stays lossless.
    fn apply_decision_inner(
        &mut self,
        path: &str,
        decision: &str,
        final_text: Option<&str>,
        merge: Option<&MergeStamp<'_>>,
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
                // A merged decision carries its own resolved label: the
                // other machine's review is the source of truth here, and
                // its teacher sidecar never travels inside a cut.
                let resolved = match (merge, decision) {
                    (Some(m), _) => m.text.to_string(),
                    (None, "accept_teacher") => self
                        .teacher
                        .get(path)
                        .and_then(|t| t["teacher_text"].as_str())
                        .context("the second opinion is not ready for this clip yet")?
                        .to_string(),
                    (None, "keep_original") => self.entries[idx]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    (None, _) => {
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
                    "decided": merge.map_or_else(iso_now, |m| m.decided.to_string()),
                });
                if let Some(src) = prev_label_source {
                    line["prev_label_source"] = Value::String(src);
                }
                if let Some(m) = merge {
                    line["merged_from"] = Value::String(m.source.to_string());
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
        if !backup.exists() && manifest.exists() {
            std::fs::copy(&manifest, &backup).context("writing manifest.jsonl.bak-review")?;
            tracing::info!(backup = %backup.display(), "one-time manifest backup taken");
        }

        // Hold the dataset lock across read → mutate → rename so a daemon
        // append can never land inside the window and be clobbered by the
        // rename (issue #114). Appends finish in microseconds; blocking
        // here is always the right call. An import already holds the lock
        // for its whole run, so it skips this acquisition rather than
        // blocking on itself.
        let _lock = if self.lock_held {
            None
        } else {
            Some(crate::dataset_lock::lock_exclusive(&self.root)?)
        };

        let contents = match std::fs::read_to_string(&manifest) {
            Ok(c) => c,
            // A dataset that has never captured anything has no manifest
            // yet; an import is allowed to create one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).context("re-reading manifest"),
        };
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

    /// Write a training-ready cut: reviewed clips only (an active
    /// keep_original, edit, or accept_teacher decision), dated `until` or
    /// earlier. Deleted and undecided clips stay out. Read-only over the
    /// dataset: the manifest and sidecars are untouched, everything new
    /// lands inside the cut directory.
    pub fn export_cut(&self, opts: &CutOptions) -> Result<CutSummary> {
        let until = match &opts.until {
            Some(s) => {
                validate_cut_date(s)?;
                s.clone()
            }
            None => today_utc(),
        };

        // Reviewed rows in manifest order. Deletes never appear here (a
        // delete removes the manifest row), but the decision check guards
        // against it anyway.
        let mut rows: Vec<(String, Value)> = Vec::new();
        let mut total_duration = 0.0;
        for entry in &self.entries {
            let Some(path) = entry["audio_filepath"].as_str() else {
                continue;
            };
            let Some(decision) = self.decisions.get(path) else {
                continue;
            };
            if !matches!(
                decision["decision"].as_str(),
                Some("keep_original" | "edit" | "accept_teacher")
            ) {
                continue;
            }
            let Some(date) = self.clip_date(path, entry) else {
                tracing::warn!(
                    clip = %path,
                    "no date for this clip (unexpected filename, no timestamp, unreadable file); leaving it out of the cut"
                );
                continue;
            };
            if date.as_str() > until.as_str() {
                continue;
            }
            total_duration += entry["duration"].as_f64().unwrap_or(0.0);
            // The manifest text IS the post-review label: decisions rewrote
            // it in place when the label changed. save_pair never writes
            // label_source, so an absent field means the original engine
            // transcript, confirmed as-is.
            rows.push((
                path.to_string(),
                json!({
                    "audio_filepath": path,
                    "duration": entry["duration"],
                    "engine": entry["engine"],
                    "text": entry["text"],
                    "label_source": entry["label_source"].as_str().unwrap_or("original"),
                    "decided": decision["decided"],
                }),
            ));
        }
        anyhow::ensure!(
            !rows.is_empty(),
            "nothing to export: no reviewed clips dated {until} or earlier"
        );

        if opts.copy_audio {
            // Self-contained cut: audio lands in <cut>/audio/<filename> and
            // the manifest rows are rewritten to resolve inside the cut dir
            // instead of the dataset root.
            let mut seen: HashMap<String, String> = HashMap::new();
            for (orig, row) in &mut rows {
                let name = Path::new(orig)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .with_context(|| format!("clip path {orig} has no filename"))?
                    .to_string();
                if let Some(prev) = seen.insert(name.clone(), orig.clone()) {
                    anyhow::bail!(
                        "two clips share the filename {name} ({prev} and {orig}); cannot copy audio into one folder"
                    );
                }
                row["audio_filepath"] = Value::String(format!("audio/{name}"));
            }
        }

        let body: String = rows.iter().map(|(_, r)| r.to_string() + "\n").collect();
        let sha = sha256_hex(body.as_bytes());

        let parent = opts
            .out_dir
            .clone()
            .unwrap_or_else(|| self.root.join("exports"));
        let parent = std::path::absolute(&parent)
            .with_context(|| format!("resolving {}", parent.display()))?;
        let dir = parent.join(format!("cut-{until}-{}", &sha[..8]));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        // The cut holds personal audio labels (and possibly audio): keep it
        // owner-only like the dataset itself.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        std::fs::write(dir.join("manifest.jsonl"), &body).context("writing the cut manifest")?;

        let dataset_root = std::path::absolute(&self.root).unwrap_or_else(|_| self.root.clone());
        let cut = json!({
            "created": iso_now(),
            "until": until,
            "dataset_root": dataset_root.display().to_string(),
            "clip_count": rows.len(),
            "total_duration_secs": total_duration,
            "manifest_sha256": sha,
        });
        std::fs::write(dir.join("cut.json"), cut.to_string()).context("writing cut.json")?;

        if opts.copy_audio {
            let audio_dir = dir.join("audio");
            std::fs::create_dir_all(&audio_dir).context("creating the cut audio dir")?;
            for (orig, row) in &rows {
                let dest = dir.join(row["audio_filepath"].as_str().unwrap_or_default());
                std::fs::copy(self.root.join(orig), &dest)
                    .with_context(|| format!("copying {orig} into the cut"))?;
            }
        }

        Ok(CutSummary {
            dir,
            until,
            clip_count: rows.len(),
            total_duration_secs: total_duration,
            manifest_sha256: sha,
        })
    }

    /// Merge a cut written on another machine into this dataset.
    ///
    /// `cut_root` is an unpacked cut folder (manifest.jsonl, cut.json, and
    /// an `audio/` folder when the cut was self-contained). The rules, in
    /// order, for every incoming row:
    ///
    ///   1. Key on `audio_filepath`. Only the file name is trusted; it is
    ///      re-anchored under this dataset's `audio/`.
    ///   2. A clip deleted locally is never resurrected: the audio is gone
    ///      on purpose, so the incoming row is skipped as a conflict.
    ///   3. Newest wins. A clip with an effective local decision keeps it
    ///      unless the incoming `decided` is strictly newer; otherwise the
    ///      row is skipped and counted, never silently overwritten. Equal
    ///      timestamps skip too, which is what makes a re-import a no-op.
    ///   4. Audio arrives only when it is missing here; existing files are
    ///      left alone. A clip with no audio on either side is skipped.
    ///   5. Decisions replay through the same path `apply_decision` uses,
    ///      so the manifest row picks up text/corrected/label_source
    ///      exactly as a local review would, and the logged line carries
    ///      `merged_from` plus the other machine's `decided`.
    ///
    /// Nothing is written when the merge has nothing to do, so a second
    /// import of the same cut leaves the dataset byte-identical.
    pub fn import_cut(&mut self, cut_root: &Path, source: &str) -> Result<ImportSummary> {
        let manifest = cut_root.join("manifest.jsonl");
        let raw = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;

        let mut summary = ImportSummary {
            source: source.to_string(),
            ..Default::default()
        };
        let mut plan: Vec<Planned> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (n, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row: Value = serde_json::from_str(line)
                .with_context(|| format!("cut manifest line {} is not valid JSON", n + 1))?;
            let Some(name) = row["audio_filepath"].as_str().and_then(cut_audio_name) else {
                tracing::warn!(
                    line = n + 1,
                    "cut manifest row has no usable audio_filepath; skipping"
                );
                summary.skipped_unusable += 1;
                continue;
            };
            let path = format!("audio/{name}");
            if !seen.insert(path.clone()) {
                tracing::warn!(clip = %path, "duplicate row in the cut; keeping the first");
                continue;
            }

            let text = row["text"].as_str().unwrap_or_default().to_string();
            let decided = row["decided"].as_str().unwrap_or_default().to_string();
            if decided.is_empty() {
                tracing::warn!(clip = %path, "cut row has no decided timestamp; skipping");
                summary.skipped_unusable += 1;
                continue;
            }

            if let Some(local) = self.decisions.get(&path) {
                if local["decision"] == "delete" {
                    tracing::info!(clip = %path, "deleted here; not restoring it from the cut");
                    summary.skipped_conflicts += 1;
                    continue;
                }
                if is_effective_decision(local)
                    && decided.as_str() <= local["decided"].as_str().unwrap_or_default()
                {
                    summary.skipped_conflicts += 1;
                    continue;
                }
            }

            let known = self.index.contains_key(&path);
            let have_audio = self.root.join(&path).exists();
            let incoming_audio = cut_audio_file(cut_root, name);
            if !have_audio && incoming_audio.is_none() {
                tracing::warn!(clip = %path, "no audio here and none in the cut; skipping");
                summary.skipped_missing_audio += 1;
                continue;
            }

            // Which decision the replay logs. A cut records the label's
            // source, not the button that was pressed, so map it back:
            // "original" means the engine transcript was confirmed as-is.
            // When that no longer matches this machine's text, record an
            // edit instead, so the manifest actually lands on the label.
            let local_text = self
                .index
                .get(&path)
                .map(|i| self.entries[*i]["text"].as_str().unwrap_or_default());
            let decision = match row["label_source"].as_str().unwrap_or("original") {
                "teacher" => "accept_teacher",
                "human" => "edit",
                _ if local_text == Some(text.as_str()) || !known => "keep_original",
                _ => "edit",
            };

            plan.push(Planned {
                path,
                name: name.to_string(),
                text,
                decision,
                decided,
                known,
                audio_from: (!have_audio).then_some(incoming_audio).flatten(),
                row,
            });
        }

        if plan.is_empty() {
            summary.finish(self);
            return Ok(summary);
        }

        // One exclusive window for the whole merge: audio copies, new
        // manifest rows, and every decision replay. The daemon's append
        // path waits (and, past its own timeout, appends anyway), so a
        // dictation saved mid-import can never be clobbered by a rewrite.
        let lock = crate::dataset_lock::lock_exclusive(&self.root)?;
        self.lock_held = true;
        let outcome = self.import_apply(&plan, source, &mut summary);
        self.lock_held = false;
        drop(lock);
        outcome?;

        summary.finish(self);
        Ok(summary)
    }

    /// The writing half of [`Store::import_cut`], run with the dataset lock
    /// held: back up, copy audio, add rows, replay decisions.
    fn import_apply(
        &mut self,
        plan: &[Planned],
        source: &str,
        summary: &mut ImportSummary,
    ) -> Result<()> {
        self.backup_before_import()?;

        let audio_dir = self.root.join("audio");
        for step in plan {
            let Some(from) = &step.audio_from else {
                continue;
            };
            std::fs::create_dir_all(&audio_dir).context("creating the dataset audio folder")?;
            let dest = audio_dir.join(&step.name);
            std::fs::copy(from, &dest)
                .with_context(|| format!("copying {} into the dataset", step.name))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
            }
            summary.audio_copied += 1;
        }

        // New clips join the manifest with the label they arrived with but
        // no review marks, so the decision replay below writes corrected
        // and label_source itself and undo can still walk back cleanly.
        let new_rows: Vec<Value> = plan
            .iter()
            .filter(|s| !s.known)
            .map(|s| {
                let mut row = json!({
                    "audio_filepath": s.path,
                    "text": s.text,
                    "duration": s.row["duration"].as_f64().unwrap_or(0.0),
                });
                if let Some(engine) = s.row["engine"].as_str() {
                    row["engine"] = Value::String(engine.to_string());
                }
                if let Some(ts) = s.row["timestamp"].as_str() {
                    row["timestamp"] = Value::String(ts.to_string());
                }
                row
            })
            .collect();
        self.append_manifest_rows(&new_rows)?;
        summary.new_clips = new_rows.len();

        for step in plan {
            let stamp = MergeStamp {
                text: &step.text,
                decided: &step.decided,
                source,
            };
            self.apply_decision_inner(&step.path, step.decision, None, Some(&stamp))
                .with_context(|| format!("merging {}", step.path))?;
            summary.imported += 1;
        }
        summary.updated_clips = summary.imported - summary.new_clips;
        Ok(())
    }

    /// Copy manifest.jsonl and decisions.jsonl aside before an import
    /// touches either. Fixed names, so the pair always describes the state
    /// right before the most recent import. A file that does not exist yet
    /// backs up as an empty one, which is the honest pre-import state and
    /// keeps "restore the backup" a complete answer either way.
    fn backup_before_import(&self) -> Result<()> {
        for name in ["manifest.jsonl", "decisions.jsonl"] {
            let src = self.root.join(name);
            let dst = self.root.join(format!("{name}.bak-import"));
            if src.exists() {
                std::fs::copy(&src, &dst).with_context(|| format!("writing {}", dst.display()))?;
            } else {
                std::fs::write(&dst, b"").with_context(|| format!("writing {}", dst.display()))?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600));
            }
        }
        Ok(())
    }

    /// Append manifest rows for clips arriving from a cut, through the same
    /// locked read + rename every other manifest write takes.
    fn append_manifest_rows(&mut self, rows: &[Value]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let encoded: Vec<String> = rows.iter().map(Value::to_string).collect();
        self.rewrite_manifest_lines(move |lines| {
            lines.extend(encoded);
            Ok(())
        })?;
        self.manifest_found = true;
        for row in rows {
            if let Some(p) = row["audio_filepath"].as_str() {
                self.index.insert(p.to_string(), self.entries.len());
                self.entries.push(row.clone());
            }
        }
        Ok(())
    }

    /// A clip's `YYYY-MM-DD`: the timestamp prefix of its filename (how
    /// rekody-core's save_pair names captures), then the row's `timestamp`
    /// field, then the audio file's mtime. All three read the same UTC
    /// clock, so the three sources agree on the date.
    fn clip_date(&self, path: &str, entry: &Value) -> Option<String> {
        if let Some(d) = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.get(..10))
            .filter(|d| validate_cut_date(d).is_ok())
        {
            return Some(d.to_string());
        }
        if let Some(d) = entry["timestamp"]
            .as_str()
            .and_then(|t| t.get(..10))
            .filter(|d| validate_cut_date(d).is_ok())
        {
            return Some(d.to_string());
        }
        let mtime = std::fs::metadata(self.root.join(path))
            .ok()?
            .modified()
            .ok()?;
        let secs = mtime
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()?
            .as_secs();
        let (y, m, d) = civil_ymd(secs);
        Some(format!("{y:04}-{m:02}-{d:02}"))
    }
}

/// How [`Store::export_cut`] runs.
pub struct CutOptions {
    /// Inclusive `YYYY-MM-DD` cutoff. `None` means today, which includes
    /// every reviewed clip recorded so far.
    pub until: Option<String>,
    /// Copy the referenced audio files into the cut so the folder is
    /// self-contained (manifest paths then resolve inside the cut dir).
    pub copy_audio: bool,
    /// Where the `cut-…` folder lands (default: `<root>/exports`).
    pub out_dir: Option<PathBuf>,
}

/// What [`Store::export_cut`] wrote.
pub struct CutSummary {
    /// Absolute path of the cut directory.
    pub dir: PathBuf,
    /// The inclusive cutoff date actually used.
    pub until: String,
    pub clip_count: usize,
    pub total_duration_secs: f64,
    /// SHA-256 (hex) of the cut's manifest.jsonl bytes.
    pub manifest_sha256: String,
}

/// Provenance for a decision replayed out of an imported cut.
struct MergeStamp<'a> {
    /// The label the other machine settled on.
    text: &'a str,
    /// That machine's `decided` timestamp, kept verbatim so the next
    /// import can still tell which side is newer.
    decided: &'a str,
    /// What the clip was merged from, stored as `merged_from`.
    source: &'a str,
}

/// One incoming clip the merge decided to apply, resolved against this
/// dataset before a single byte is written.
struct Planned {
    /// This dataset's key for the clip (`audio/<name>`).
    path: String,
    /// Bare audio file name, already through the allowlist.
    name: String,
    /// The label the cut carries.
    text: String,
    /// Which decision the replay logs.
    decision: &'static str,
    /// The other machine's review time.
    decided: String,
    /// Whether this dataset already has a manifest row for the clip.
    known: bool,
    /// Where to copy the audio from, when this dataset lacks it.
    audio_from: Option<PathBuf>,
    /// The cut's raw row, for the fields a new manifest row needs.
    row: Value,
}

/// What [`Store::import_cut`] did, for the CLI line and the page's note.
#[derive(Debug, Default, Clone)]
pub struct ImportSummary {
    /// What the cut was identified as (`cut-<date>-<hash8>` when known).
    pub source: String,
    /// Decisions replayed into this dataset.
    pub imported: usize,
    /// Of those, clips this machine had never seen.
    pub new_clips: usize,
    /// Of those, clips that already existed here.
    pub updated_clips: usize,
    /// Clips left alone because this machine's review is newer, the same,
    /// or because the clip was deleted here on purpose.
    pub skipped_conflicts: usize,
    /// Clips with no audio on either side.
    pub skipped_missing_audio: usize,
    /// Rows the cut could not describe well enough to merge.
    pub skipped_unusable: usize,
    /// Audio files copied in.
    pub audio_copied: usize,
    /// Reviewed clips in this dataset after the merge.
    pub reviewed_clips: usize,
    /// Reviewed audio in this dataset after the merge.
    pub reviewed_duration_secs: f64,
}

impl ImportSummary {
    /// Stamp the after-the-merge dataset totals.
    fn finish(&mut self, store: &Store) {
        self.reviewed_clips = store.decision_count();
        self.reviewed_duration_secs = store.reviewed_duration_secs();
    }

    /// Every clip the merge left out, for one plain-language line.
    pub fn skipped(&self) -> usize {
        self.skipped_conflicts + self.skipped_missing_audio + self.skipped_unusable
    }

    /// The summary as JSON, for the page and the CLI's stdout line.
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source,
            "imported": self.imported,
            "new_clips": self.new_clips,
            "updated_clips": self.updated_clips,
            "skipped_conflicts": self.skipped_conflicts,
            "skipped_missing_audio": self.skipped_missing_audio,
            "skipped_unusable": self.skipped_unusable,
            "audio_copied": self.audio_copied,
            "reviewed_clips": self.reviewed_clips,
            "reviewed_duration_secs": self.reviewed_duration_secs,
            "reviewed_label": fmt_reviewed_duration(self.reviewed_duration_secs),
        })
    }
}

/// A decision that leaves a clip reviewed and still in the dataset. Deletes
/// are decisions too, but they take the clip (and its audio) out.
fn is_effective_decision(d: &Value) -> bool {
    matches!(
        d["decision"].as_str(),
        Some("keep_original" | "edit" | "accept_teacher")
    )
}

/// Reviewed audio in plain language: minutes below an hour, one decimal at
/// or above ("48 minutes", "1 minute", "3.2 hours"). The boundary is the
/// rounded minute count, so 59.9 minutes reads "1.0 hours" instead of the
/// odd "60 minutes".
pub fn fmt_reviewed_duration(secs: f64) -> String {
    let secs = if secs.is_finite() && secs > 0.0 {
        secs
    } else {
        0.0
    };
    let minutes = (secs / 60.0).round();
    if minutes < 60.0 {
        let m = minutes as u64;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else {
        format!("{:.1} hours", secs / 3600.0)
    }
}

/// The bare, trusted audio file name behind a cut row's `audio_filepath`.
/// Only the file name travels: a cut is a file someone was handed, so its
/// paths never get to say where anything lands. Same allowlist the audio
/// route uses (capture names are timestamps plus hex).
pub(crate) fn cut_audio_name(raw: &str) -> Option<&str> {
    let name = raw.rsplit(['/', '\\']).next()?;
    if name.is_empty() || name.starts_with('.') || name.contains("..") {
        return None;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return None;
    }
    (name.ends_with(".flac") || name.ends_with(".wav")).then_some(name)
}

/// Where a cut keeps a clip's audio: `audio/<name>` for a self-contained
/// cut, with a flat layout accepted too. `None` when the cut has none.
fn cut_audio_file(cut_root: &Path, name: &str) -> Option<PathBuf> {
    [cut_root.join("audio").join(name), cut_root.join(name)]
        .into_iter()
        .find(|p| p.is_file())
}

/// Accept exactly `YYYY-MM-DD` with a sane month and day; lexicographic
/// comparison then matches chronological order.
fn validate_cut_date(s: &str) -> Result<()> {
    let b = s.as_bytes();
    let shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit());
    let month: u32 = if shape {
        s[5..7].parse().unwrap_or(0)
    } else {
        0
    };
    let day: u32 = if shape {
        s[8..10].parse().unwrap_or(0)
    } else {
        0
    };
    anyhow::ensure!(
        shape && (1..=12).contains(&month) && (1..=31).contains(&day),
        "dates look like YYYY-MM-DD (got {s:?})"
    );
    Ok(())
}

/// Today's UTC date as `YYYY-MM-DD`, the same clock save_pair stamps
/// filenames with.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, m, d) = civil_ymd(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// SHA-256 of `bytes` as lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
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
    let tod = secs % 86400;
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mth, d) = civil_ymd(secs);
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Civil UTC `(year, month, day)` for a Unix timestamp: Howard Hinnant's
/// civil_from_days, shared by `iso_now`, `today_utc`, and the export date
/// fallback.
fn civil_ymd(secs: u64) -> (i64, u64, u64) {
    let days = secs / 86400;
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
    (y, mth, d)
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

    /// Issue #114 repro: a daemon-style append landing while a decision
    /// rewrite sits between its read and its rename must survive. The
    /// mutate closure runs exactly inside that window, so sleeping there
    /// makes the race deterministic; the appender uses the same bounded
    /// lock the daemon uses and must block until the rename finishes.
    #[test]
    fn concurrent_append_survives_decision_rewrite() {
        let dir = seed_dataset();
        let root = dir.path().to_path_buf();
        let mut store = Store::load(root.clone()).unwrap();

        let appender = {
            let root = root.clone();
            std::thread::spawn(move || {
                // Land inside the rewrite's read → rename window.
                std::thread::sleep(std::time::Duration::from_millis(120));
                let lock = crate::dataset_lock::lock_with_timeout(
                    &root,
                    std::time::Duration::from_secs(5),
                )
                .unwrap();
                assert!(
                    lock.is_some(),
                    "append should win the lock once the rewrite ends"
                );
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(root.join("manifest.jsonl"))
                    .unwrap();
                writeln!(
                    f,
                    "{}",
                    json!({
                        "audio_filepath": "audio/new.flac",
                        "text": "appended mid rewrite",
                        "duration": 1.0,
                        "engine": "nemotron",
                        "timestamp": "2026-07-01T10-02-00",
                    })
                )
                .unwrap();
            })
        };

        store
            .rewrite_manifest_lines(|lines| {
                // Hold the window open long past the appender's start.
                std::thread::sleep(std::time::Duration::from_millis(400));
                lines[0] = {
                    let mut v: Value = serde_json::from_str(&lines[0]).unwrap();
                    v["text"] = Value::String("rewritten".into());
                    v.to_string()
                };
                Ok(())
            })
            .unwrap();
        appender.join().unwrap();

        let body = std::fs::read_to_string(root.join("manifest.jsonl")).unwrap();
        assert!(body.contains("rewritten"), "rewrite must land");
        assert!(
            body.contains("appended mid rewrite"),
            "concurrent append must survive the rewrite rename"
        );
        assert_eq!(
            body.lines().count(),
            3,
            "both original rows plus the append"
        );
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

    // ---- export cuts ----

    /// Three clips with save_pair-shaped dated filenames and audio on disk:
    /// June 1, June 15, and July 2.
    fn seed_dated_with_audio() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        let mut lines = Vec::new();
        for (stamp, text) in [
            ("2026-06-01T10-00-00", "alpha one"),
            ("2026-06-15T11-00-00", "bravo two"),
            ("2026-07-02T12-00-00", "charlie three"),
        ] {
            let name = format!("{stamp}-aaaa-111.flac");
            lines.push(
                json!({
                    "audio_filepath": format!("audio/{name}"),
                    "text": text,
                    "duration": 2.0,
                    "engine": "nemotron",
                    "timestamp": stamp,
                })
                .to_string(),
            );
            std::fs::write(audio.join(&name), format!("fLaC-{stamp}")).unwrap();
        }
        std::fs::write(dir.path().join("manifest.jsonl"), lines.join("\n") + "\n").unwrap();
        dir
    }

    fn cut_manifest_rows(cut_dir: &Path) -> Vec<Value> {
        std::fs::read_to_string(cut_dir.join("manifest.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn export_includes_reviewed_clips_only_and_is_read_only() {
        let dir = seed_dated_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .apply_decision(
                "audio/2026-06-01T10-00-00-aaaa-111.flac",
                "edit",
                Some("alpha won"),
            )
            .unwrap();
        store
            .apply_decision("audio/2026-07-02T12-00-00-aaaa-111.flac", "delete", None)
            .unwrap();
        // June 15 stays undecided.
        let manifest_before = std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap();
        let decisions_before = std::fs::read_to_string(dir.path().join("decisions.jsonl")).unwrap();

        let cut = store
            .export_cut(&CutOptions {
                until: None,
                copy_audio: false,
                out_dir: None,
            })
            .unwrap();

        assert_eq!(cut.clip_count, 1);
        assert!((cut.total_duration_secs - 2.0).abs() < 1e-9);
        assert!(cut.dir.is_absolute());
        assert!(cut.dir.starts_with(dir.path().join("exports")));

        let rows = cut_manifest_rows(&cut.dir);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["audio_filepath"],
            "audio/2026-06-01T10-00-00-aaaa-111.flac"
        );
        assert_eq!(
            rows[0]["text"], "alpha won",
            "the post-review label is exported"
        );
        assert_eq!(rows[0]["label_source"], "human");
        assert_eq!(rows[0]["engine"], "nemotron");
        assert!(rows[0]["decided"].as_str().is_some());

        let cut_json: Value =
            serde_json::from_str(&std::fs::read_to_string(cut.dir.join("cut.json")).unwrap())
                .unwrap();
        assert_eq!(cut_json["clip_count"], 1);
        assert_eq!(cut_json["until"], cut.until);
        assert_eq!(cut_json["manifest_sha256"], cut.manifest_sha256);
        assert!(cut_json["created"].as_str().is_some());
        assert!(cut_json["dataset_root"].as_str().is_some());

        // Export is read-only over the dataset.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap(),
            manifest_before
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("decisions.jsonl")).unwrap(),
            decisions_before
        );
    }

    #[test]
    fn export_filters_by_inclusive_date() {
        let dir = seed_dated_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        for name in [
            "audio/2026-06-01T10-00-00-aaaa-111.flac",
            "audio/2026-06-15T11-00-00-aaaa-111.flac",
            "audio/2026-07-02T12-00-00-aaaa-111.flac",
        ] {
            store.apply_decision(name, "keep_original", None).unwrap();
        }

        // Inclusive: June 15 makes the cut, July 2 does not.
        let cut = store
            .export_cut(&CutOptions {
                until: Some("2026-06-15".into()),
                copy_audio: false,
                out_dir: None,
            })
            .unwrap();
        assert_eq!(cut.clip_count, 2);
        assert_eq!(cut.until, "2026-06-15");
        let rows = cut_manifest_rows(&cut.dir);
        assert!(
            rows.iter()
                .all(|r| { r["audio_filepath"].as_str().unwrap() <= "audio/2026-06-15T99" })
        );
        // keep_original rows without a label_source report the original.
        assert!(rows.iter().all(|r| r["label_source"] == "original"));

        // A date before every clip: nothing to export is an error, not an
        // empty folder.
        assert!(
            store
                .export_cut(&CutOptions {
                    until: Some("2026-05-31".into()),
                    copy_audio: false,
                    out_dir: None,
                })
                .is_err()
        );

        // Garbage dates are rejected before anything is written.
        for bad in ["garbage", "2026-13-01", "2026-6-1", "2026/06/01", ""] {
            assert!(
                store
                    .export_cut(&CutOptions {
                        until: Some(bad.into()),
                        copy_audio: false,
                        out_dir: None,
                    })
                    .is_err(),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn export_manifest_sha_is_stable_across_reloads() {
        let dir = seed_dated_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .apply_decision(
                "audio/2026-06-01T10-00-00-aaaa-111.flac",
                "edit",
                Some("alpha won"),
            )
            .unwrap();
        store
            .apply_decision(
                "audio/2026-06-15T11-00-00-aaaa-111.flac",
                "keep_original",
                None,
            )
            .unwrap();

        let opts = CutOptions {
            until: Some("2026-07-27".into()),
            copy_audio: false,
            out_dir: None,
        };
        let first = store.export_cut(&opts).unwrap();
        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        let second = reloaded.export_cut(&opts).unwrap();

        assert_eq!(first.manifest_sha256, second.manifest_sha256);
        assert_eq!(first.dir, second.dir, "same content, same cut folder");
        // The recorded checksum matches the bytes on disk.
        let body = std::fs::read(first.dir.join("manifest.jsonl")).unwrap();
        assert_eq!(sha256_hex(&body), first.manifest_sha256);
        assert!(
            first
                .dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(&first.manifest_sha256[..8])
        );
    }

    #[test]
    fn export_copy_audio_rewrites_paths_and_is_self_contained() {
        let dir = seed_dated_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        store
            .apply_decision(
                "audio/2026-06-01T10-00-00-aaaa-111.flac",
                "edit",
                Some("alpha won"),
            )
            .unwrap();
        store
            .apply_decision(
                "audio/2026-06-15T11-00-00-aaaa-111.flac",
                "keep_original",
                None,
            )
            .unwrap();

        let out = tempfile::tempdir().unwrap();
        let cut = store
            .export_cut(&CutOptions {
                until: None,
                copy_audio: true,
                out_dir: Some(out.path().to_path_buf()),
            })
            .unwrap();

        // --out relocates the cut folder.
        assert!(cut.dir.starts_with(out.path()));
        let rows = cut_manifest_rows(&cut.dir);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let rel = row["audio_filepath"].as_str().unwrap();
            assert!(
                rel.starts_with("audio/"),
                "rewritten relative to the cut dir"
            );
            let copied = cut.dir.join(rel);
            assert!(
                copied.exists(),
                "self-contained: {rel} lives inside the cut"
            );
            // Bytes match the dataset original of the same filename.
            let name = Path::new(rel).file_name().unwrap();
            let original = dir.path().join("audio").join(name);
            assert_eq!(
                std::fs::read(&copied).unwrap(),
                std::fs::read(&original).unwrap()
            );
        }
    }

    // ── Hours reviewed ───────────────────────────────────────────────────

    #[test]
    fn reviewed_hours_count_decided_clips_only_and_never_deletes() {
        let dir = seed_dated_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        let paths: Vec<String> = store
            .entries
            .iter()
            .map(|e| e["audio_filepath"].as_str().unwrap().to_string())
            .collect();

        // Three 2 s clips, none reviewed yet.
        assert_eq!(store.total_duration_secs(), 6.0);
        assert_eq!(store.reviewed_duration_secs(), 0.0);

        store
            .apply_decision(&paths[0], "keep_original", None)
            .unwrap();
        assert_eq!(store.reviewed_duration_secs(), 2.0);
        store
            .apply_decision(&paths[1], "edit", Some("bravo too"))
            .unwrap();
        assert_eq!(store.reviewed_duration_secs(), 4.0);

        // A delete is a decision, but its audio is gone: it never counts.
        store.apply_decision(&paths[2], "delete", None).unwrap();
        assert_eq!(store.reviewed_duration_secs(), 4.0);

        // Undo takes its clip back out of the number.
        store.apply_decision(&paths[0], "undo", None).unwrap();
        assert_eq!(store.reviewed_duration_secs(), 2.0);

        // And it all survives a reload from the log.
        let reloaded = Store::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.reviewed_duration_secs(), 2.0);
    }

    #[test]
    fn reviewed_duration_reads_as_minutes_then_hours() {
        // Below an hour: whole minutes, singular where it belongs.
        assert_eq!(fmt_reviewed_duration(0.0), "0 minutes");
        assert_eq!(fmt_reviewed_duration(45.0), "1 minute");
        assert_eq!(fmt_reviewed_duration(90.0), "2 minutes");
        assert_eq!(fmt_reviewed_duration(2880.0), "48 minutes");
        // The boundary: 59 minutes still reads in minutes, and anything
        // that would round to 60 crosses over rather than saying "60
        // minutes" right before "1.0 hours".
        assert_eq!(fmt_reviewed_duration(59.0 * 60.0), "59 minutes");
        assert_eq!(fmt_reviewed_duration(59.6 * 60.0), "1.0 hours");
        assert_eq!(fmt_reviewed_duration(3600.0), "1.0 hours");
        // At or above an hour: one decimal, always.
        assert_eq!(fmt_reviewed_duration(3600.0 * 3.2), "3.2 hours");
        assert_eq!(fmt_reviewed_duration(3600.0 * 12.34), "12.3 hours");
        // Nothing sensible in, "0 minutes" out.
        assert_eq!(fmt_reviewed_duration(-5.0), "0 minutes");
        assert_eq!(fmt_reviewed_duration(f64::NAN), "0 minutes");
    }

    // ── Import / merge ───────────────────────────────────────────────────

    /// Review two clips on "machine A" and export a self-contained cut.
    fn machine_a_cut() -> (tempfile::TempDir, PathBuf, Vec<String>) {
        let dir = seed_dated_with_audio();
        let mut store = Store::load(dir.path().to_path_buf()).unwrap();
        let paths: Vec<String> = store
            .entries
            .iter()
            .map(|e| e["audio_filepath"].as_str().unwrap().to_string())
            .collect();
        store
            .apply_decision(&paths[0], "edit", Some("alpha won"))
            .unwrap();
        store
            .apply_decision(&paths[1], "keep_original", None)
            .unwrap();
        let cut = store
            .export_cut(&CutOptions {
                until: None,
                copy_audio: true,
                out_dir: None,
            })
            .unwrap();
        (dir, cut.dir, paths)
    }

    /// A fresh dataset holding only the third clip, standing in for the
    /// other machine.
    fn machine_b() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("audio")).unwrap();
        let name = "2026-07-02T12-00-00-aaaa-111.flac";
        std::fs::write(
            dir.path().join("audio").join(name),
            "fLaC-2026-07-02T12-00-00",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("manifest.jsonl"),
            json!({
                "audio_filepath": format!("audio/{name}"),
                "text": "charlie three",
                "duration": 2.0,
                "engine": "nemotron",
                "timestamp": "2026-07-02T12-00-00",
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn import_brings_over_text_and_audio_and_is_idempotent() {
        let (_a, cut, paths) = machine_a_cut();
        let b = machine_b();
        let mut store = Store::load(b.path().to_path_buf()).unwrap();
        assert_eq!(store.clip_count(), 1);

        let first = store.import_cut(&cut, "cut-under-test").unwrap();
        assert_eq!(first.imported, 2);
        assert_eq!(first.new_clips, 2);
        assert_eq!(first.audio_copied, 2);
        assert_eq!(first.skipped_conflicts, 0);
        assert_eq!(first.reviewed_clips, 2);
        assert_eq!(first.reviewed_duration_secs, 4.0);

        // The corrected text landed in the manifest, marked as a human
        // label, and the audio came with it.
        let entry = manifest_entry(b.path(), &paths[0]);
        assert_eq!(entry["text"], "alpha won");
        assert_eq!(entry["corrected"], true);
        assert_eq!(entry["label_source"], "human");
        assert!(b.path().join(&paths[0]).exists());
        // The confirmed-as-is clip arrived too, without a rewrite mark.
        let kept = manifest_entry(b.path(), &paths[1]);
        assert_eq!(kept["text"], "bravo two");
        assert!(kept.get("corrected").is_none());
        // Provenance rides on the merged decision lines.
        let log = std::fs::read_to_string(b.path().join("decisions.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(
            log.lines()
                .all(|l| l.contains("\"merged_from\":\"cut-under-test\""))
        );
        // Both backups exist, from before the merge wrote anything.
        assert!(b.path().join("manifest.jsonl.bak-import").exists());

        // Running the same cut again changes nothing at all.
        let manifest_before = std::fs::read(b.path().join("manifest.jsonl")).unwrap();
        let log_before = std::fs::read(b.path().join("decisions.jsonl")).unwrap();
        let again = store.import_cut(&cut, "cut-under-test").unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.audio_copied, 0);
        assert_eq!(again.skipped_conflicts, 2);
        assert_eq!(again.reviewed_clips, 2);
        assert_eq!(
            std::fs::read(b.path().join("manifest.jsonl")).unwrap(),
            manifest_before
        );
        assert_eq!(
            std::fs::read(b.path().join("decisions.jsonl")).unwrap(),
            log_before
        );

        // And a reload agrees with the in-memory state.
        let reloaded = Store::load(b.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.decision_count(), 2);
        assert_eq!(reloaded.reviewed_duration_secs(), 4.0);
    }

    #[test]
    fn newest_review_wins_and_the_older_one_is_reported_not_applied() {
        let (_a, cut, paths) = machine_a_cut();
        let b = machine_b();
        let mut store = Store::load(b.path().to_path_buf()).unwrap();
        store.import_cut(&cut, "cut-one").unwrap();

        // Rewrite the cut so one row is newer than the local decision and
        // the other is older. Everything else stays byte-identical.
        let raw = std::fs::read_to_string(cut.join("manifest.jsonl")).unwrap();
        let mut rows: Vec<Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        rows[0]["text"] = Value::String("alpha one, newer".into());
        rows[0]["decided"] = Value::String("2099-01-01T00:00:00Z".into());
        rows[1]["text"] = Value::String("bravo two, stale".into());
        rows[1]["decided"] = Value::String("2000-01-01T00:00:00Z".into());
        std::fs::write(
            cut.join("manifest.jsonl"),
            rows.iter()
                .map(|r| r.to_string() + "\n")
                .collect::<String>(),
        )
        .unwrap();

        let merged = store.import_cut(&cut, "cut-two").unwrap();
        assert_eq!(merged.imported, 1, "only the newer row should apply");
        assert_eq!(merged.updated_clips, 1);
        assert_eq!(merged.new_clips, 0);
        assert_eq!(merged.skipped_conflicts, 1, "the stale row is reported");
        assert_eq!(merged.audio_copied, 0, "audio was already here");

        assert_eq!(
            manifest_entry(b.path(), &paths[0])["text"],
            "alpha one, newer"
        );
        assert_eq!(
            manifest_entry(b.path(), &paths[1])["text"],
            "bravo two",
            "the stale row must not overwrite the newer local label"
        );
    }

    #[test]
    fn a_clip_deleted_here_is_never_restored_by_an_import() {
        let (_a, cut, paths) = machine_a_cut();
        let b = machine_b();
        let mut store = Store::load(b.path().to_path_buf()).unwrap();
        store.import_cut(&cut, "cut-one").unwrap();

        store.apply_decision(&paths[0], "delete", None).unwrap();
        assert!(!b.path().join(&paths[0]).exists());

        let again = store.import_cut(&cut, "cut-one").unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped_conflicts, 2);
        assert!(
            !b.path().join(&paths[0]).exists(),
            "the audio must stay deleted"
        );
        assert!(
            !std::fs::read_to_string(b.path().join("manifest.jsonl"))
                .unwrap()
                .contains("alpha won")
        );
    }

    #[test]
    fn a_cut_without_audio_only_merges_clips_this_machine_already_has() {
        let dir = seed_dated_with_audio();
        let mut a = Store::load(dir.path().to_path_buf()).unwrap();
        let paths: Vec<String> = a
            .entries
            .iter()
            .map(|e| e["audio_filepath"].as_str().unwrap().to_string())
            .collect();
        a.apply_decision(&paths[0], "edit", Some("alpha won"))
            .unwrap();
        a.apply_decision(&paths[2], "edit", Some("charlie tree"))
            .unwrap();
        let cut = a
            .export_cut(&CutOptions {
                until: None,
                copy_audio: false,
                out_dir: None,
            })
            .unwrap();

        let b = machine_b();
        let mut store = Store::load(b.path().to_path_buf()).unwrap();
        let summary = store.import_cut(&cut.dir, "cut-no-audio").unwrap();
        assert_eq!(summary.imported, 1, "only the clip b already holds");
        assert_eq!(summary.audio_copied, 0);
        assert_eq!(summary.skipped_missing_audio, 1);
        assert_eq!(manifest_entry(b.path(), &paths[2])["text"], "charlie tree");
    }
}
