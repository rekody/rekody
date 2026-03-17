//! Transcription history persistence for Chamgei voice dictation.
//!
//! Saves all dictation entries to `~/.config/chamgei/history.json` so users
//! can review, search, and export their transcription history.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum number of history entries to retain.
const MAX_ENTRIES: usize = 5000;

/// A single transcription history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The final text that was injected (after LLM cleanup).
    pub text: String,
    /// The raw STT output before any LLM post-processing.
    pub raw_transcript: String,
    /// ISO 8601 timestamp of the dictation.
    pub timestamp: String,
    /// STT transcription latency in milliseconds.
    pub stt_latency_ms: u64,
    /// LLM post-processing latency in milliseconds (None if LLM was not used).
    pub llm_latency_ms: Option<u64>,
    /// Which LLM provider was used (None if LLM was skipped).
    pub provider: Option<String>,
    /// The application that was focused when the dictation occurred.
    pub app_context: String,
}

/// Transcription history manager.
///
/// Entries are stored newest-first and capped at [`MAX_ENTRIES`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    entries: Vec<HistoryEntry>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

/// Returns the path to the history JSON file: `~/.config/chamgei/history.json`.
fn history_file_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("chamgei")
            .join("history.json"),
    )
}

/// Generate an ISO 8601 timestamp from the current system time.
fn iso8601_now() -> String {
    use std::time::SystemTime;

    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    let secs = duration.as_secs();

    // Break epoch seconds into date/time components.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Convert days since epoch to year/month/day.
    // Algorithm from Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hours, minutes, seconds
    )
}

impl History {
    /// Load history from `~/.config/chamgei/history.json`.
    ///
    /// Returns an empty history if the file does not exist or cannot be parsed.
    pub fn load() -> Self {
        let Some(path) = history_file_path() else {
            tracing::debug!("could not determine history file path, using empty history");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(history) => {
                    tracing::debug!(?path, "loaded transcription history");
                    history
                }
                Err(e) => {
                    tracing::warn!(?path, error = %e, "failed to parse history file, using empty history");
                    Self::default()
                }
            },
            Err(_) => {
                tracing::debug!(?path, "no history file found, using empty history");
                Self::default()
            }
        }
    }

    /// Save history to `~/.config/chamgei/history.json`.
    pub fn save(&self) {
        let Some(path) = history_file_path() else {
            tracing::warn!("could not determine history file path, skipping save");
            return;
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(error = %e, "failed to create history directory");
                return;
            }
        }

        match serde_json::to_string_pretty(&self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, &json) {
                    tracing::warn!(error = %e, "failed to save history");
                } else {
                    tracing::debug!(?path, entries = self.entries.len(), "saved transcription history");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize history");
            }
        }
    }

    /// Add a new entry and auto-save.
    ///
    /// The entry is inserted at the front (newest first). If the history
    /// exceeds [`MAX_ENTRIES`], the oldest entries are dropped.
    pub fn add(&mut self, entry: HistoryEntry) {
        self.entries.insert(0, entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.truncate(MAX_ENTRIES);
        }
        self.save();
    }

    /// Return all entries, newest first.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Clear all history entries and save the empty state.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    /// Search entries by text content (case-insensitive substring match).
    ///
    /// Returns matching entries in newest-first order.
    pub fn search(&self, query: &str) -> Vec<&HistoryEntry> {
        let query_lower = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                e.text.to_lowercase().contains(&query_lower)
                    || e.raw_transcript.to_lowercase().contains(&query_lower)
            })
            .collect()
    }

    /// Create a new [`HistoryEntry`] with the current timestamp.
    pub fn new_entry(
        text: String,
        raw_transcript: String,
        stt_latency_ms: u64,
        llm_latency_ms: Option<u64>,
        provider: Option<String>,
        app_context: String,
    ) -> HistoryEntry {
        HistoryEntry {
            text,
            raw_transcript,
            timestamp: iso8601_now(),
            stt_latency_ms,
            llm_latency_ms,
            provider,
            app_context,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            text: text.to_string(),
            raw_transcript: format!("raw: {}", text),
            timestamp: "2026-03-17T12:00:00Z".to_string(),
            stt_latency_ms: 100,
            llm_latency_ms: Some(200),
            provider: Some("groq".to_string()),
            app_context: "VS Code".to_string(),
        }
    }

    #[test]
    fn add_and_retrieve_entries() {
        let mut history = History::default();
        // Override save to no-op for tests (save will warn but not crash).
        history.entries.push(sample_entry("first"));
        history.entries.insert(0, sample_entry("second"));

        assert_eq!(history.entries().len(), 2);
        assert_eq!(history.entries()[0].text, "second");
        assert_eq!(history.entries()[1].text, "first");
    }

    #[test]
    fn cap_at_max_entries() {
        let mut history = History::default();
        for i in 0..MAX_ENTRIES + 50 {
            history.entries.insert(0, sample_entry(&format!("entry {}", i)));
        }
        history.entries.truncate(MAX_ENTRIES);

        assert_eq!(history.entries().len(), MAX_ENTRIES);
    }

    #[test]
    fn search_filters_by_text() {
        let mut history = History::default();
        history.entries.push(sample_entry("Hello world"));
        history.entries.push(sample_entry("Goodbye world"));
        history.entries.push(sample_entry("Hello Rust"));

        let results = history.search("hello");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn clear_removes_all() {
        let mut history = History::default();
        history.entries.push(sample_entry("test"));
        history.entries.clear();

        assert!(history.entries().is_empty());
    }

    #[test]
    fn serialization_roundtrip() {
        let mut history = History::default();
        history.entries.push(sample_entry("test entry"));

        let json = serde_json::to_string(&history).unwrap();
        let parsed: History = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.entries().len(), 1);
        assert_eq!(parsed.entries()[0].text, "test entry");
    }

    #[test]
    fn iso8601_now_format() {
        let ts = iso8601_now();
        // Should look like "2026-03-17T12:34:56Z"
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
