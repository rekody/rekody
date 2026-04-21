//! Auto-learning from user corrections to dictation output.
//!
//! Tracks corrections the user makes after rekody injects text, analyses
//! them for recurring patterns, and feeds those patterns back into the LLM
//! prompt so future dictation reflects the user's preferences.
//!
//! Example pattern: "user always changes 'their' to 'there' in code editors."

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maximum number of correction entries kept in the log.
const MAX_LOG_ENTRIES: usize = 1000;

// ---------------------------------------------------------------------------
// CorrectionEntry
// ---------------------------------------------------------------------------

/// A single correction made by the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CorrectionEntry {
    /// What rekody originally produced.
    pub original: String,
    /// What the user changed it to.
    pub corrected: String,
    /// Which application the correction was made in (e.g. "VS Code").
    pub app_context: String,
    /// Unix timestamp (seconds since epoch) when the correction occurred.
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// CorrectionLog
// ---------------------------------------------------------------------------

/// Persistent log of user corrections, capped at [`MAX_LOG_ENTRIES`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CorrectionLog {
    #[serde(default)]
    entries: Vec<CorrectionEntry>,
}

impl CorrectionLog {
    /// Create a new, empty correction log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the default file path: `~/.config/rekody/corrections.toml`.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join("rekody")
            .join("corrections.toml"))
    }

    /// Load a correction log from a TOML file.
    ///
    /// If the file does not exist an empty log is returned and the file is
    /// created so the user has a starting point.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!(path = %path.display(), "corrections file not found, creating default");
            let log = Self::new();
            log.save(path)?;
            return Ok(log);
        }

        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read corrections file: {}", path.display()))?;
        let log: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse corrections file: {}", path.display()))?;
        tracing::info!(
            path = %path.display(),
            entry_count = log.entries.len(),
            "loaded correction log"
        );
        Ok(log)
    }

    /// Save the correction log to a TOML file.
    ///
    /// Parent directories are created automatically if they do not exist.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create corrections directory: {}",
                    parent.display()
                )
            })?;
        }

        let contents =
            toml::to_string_pretty(self).context("failed to serialize corrections to TOML")?;
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write corrections file: {}", path.display()))?;
        tracing::debug!(path = %path.display(), "corrections saved");
        Ok(())
    }

    /// Record a new correction. If the log exceeds [`MAX_LOG_ENTRIES`] the
    /// oldest entries are dropped.
    pub fn record(
        &mut self,
        original: impl Into<String>,
        corrected: impl Into<String>,
        app_context: impl Into<String>,
        timestamp: u64,
    ) {
        self.entries.push(CorrectionEntry {
            original: original.into(),
            corrected: corrected.into(),
            app_context: app_context.into(),
            timestamp,
        });

        // Enforce size limit by removing the oldest entries.
        if self.entries.len() > MAX_LOG_ENTRIES {
            let excess = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..excess);
        }
    }

    /// Return a slice of all logged correction entries.
    pub fn entries(&self) -> &[CorrectionEntry] {
        &self.entries
    }
}

// ---------------------------------------------------------------------------
// CorrectionPattern
// ---------------------------------------------------------------------------

/// A recurring correction pattern discovered by analysing the log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CorrectionPattern {
    /// The text rekody originally produced.
    pub from_text: String,
    /// The text the user consistently corrects it to.
    pub to_text: String,
    /// How many times this substitution has been observed.
    pub frequency: usize,
    /// The application context where this pattern is most common (empty string
    /// if it spans multiple apps equally).
    pub context: String,
}

// ---------------------------------------------------------------------------
// CorrectionPatterns (analyser)
// ---------------------------------------------------------------------------

/// Analyses a [`CorrectionLog`] to discover recurring [`CorrectionPattern`]s.
pub struct CorrectionPatterns;

impl CorrectionPatterns {
    /// Analyse the correction log and return patterns sorted by frequency
    /// (most frequent first).
    ///
    /// A correction pair must appear at least **2** times to be considered a
    /// pattern — single occurrences are treated as one-off typos.
    pub fn analyze(log: &CorrectionLog) -> Vec<CorrectionPattern> {
        // Group by (original, corrected) → per-app counts.
        let mut groups: HashMap<(String, String), HashMap<String, usize>> = HashMap::new();

        for entry in &log.entries {
            let key = (entry.original.clone(), entry.corrected.clone());
            let app_counts = groups.entry(key).or_default();
            *app_counts.entry(entry.app_context.clone()).or_insert(0) += 1;
        }

        let mut patterns: Vec<CorrectionPattern> = groups
            .into_iter()
            .filter_map(|((from_text, to_text), app_counts)| {
                let total: usize = app_counts.values().sum();
                if total < 2 {
                    return None;
                }

                // Pick the most frequent app context. If there is a tie or
                // the corrections span many apps, use an empty string.
                let dominant_context = app_counts
                    .iter()
                    .max_by_key(|&(_, count)| *count)
                    .map(|(app, &count)| {
                        // Only attribute to a specific app if it accounts for
                        // more than half the occurrences.
                        if count * 2 > total {
                            app.clone()
                        } else {
                            String::new()
                        }
                    })
                    .unwrap_or_default();

                Some(CorrectionPattern {
                    from_text,
                    to_text,
                    frequency: total,
                    context: dominant_context,
                })
            })
            .collect();

        patterns.sort_by_key(|p| std::cmp::Reverse(p.frequency));
        patterns
    }
}

// ---------------------------------------------------------------------------
// Prompt injection
// ---------------------------------------------------------------------------

/// Append learned correction patterns to a base LLM system prompt.
///
/// If `patterns` is empty the base prompt is returned unchanged. Otherwise a
/// section describing the user's correction preferences is appended so the
/// LLM can avoid repeating the same mistakes.
pub fn inject_correction_hints(base_prompt: &str, patterns: &[CorrectionPattern]) -> String {
    if patterns.is_empty() {
        return base_prompt.to_owned();
    }

    let hint_lines: Vec<String> = patterns
        .iter()
        .map(|p| {
            if p.context.is_empty() {
                format!(
                    "  - Change \"{}\" to \"{}\" (observed {} times)",
                    p.from_text, p.to_text, p.frequency
                )
            } else {
                format!(
                    "  - Change \"{}\" to \"{}\" in {} (observed {} times)",
                    p.from_text, p.to_text, p.context, p.frequency
                )
            }
        })
        .collect();

    format!(
        "{base_prompt}\n\n\
         Learned user preferences — apply these corrections automatically:\n\
         {}",
        hint_lines.join("\n")
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_retrieve() {
        let mut log = CorrectionLog::new();
        log.record("their", "there", "VS Code", 1000);
        log.record("its", "it's", "Slack", 1001);
        assert_eq!(log.entries().len(), 2);
        assert_eq!(log.entries()[0].original, "their");
        assert_eq!(log.entries()[1].corrected, "it's");
    }

    #[test]
    fn log_enforces_max_entries() {
        let mut log = CorrectionLog::new();
        for i in 0..1050 {
            log.record(format!("word{i}"), format!("fix{i}"), "TestApp", i as u64);
        }
        assert_eq!(log.entries().len(), MAX_LOG_ENTRIES);
        // Oldest entries should have been dropped — first entry is word50.
        assert_eq!(log.entries()[0].original, "word50");
    }

    #[test]
    fn analyze_finds_patterns() {
        let mut log = CorrectionLog::new();
        // "their" → "there" three times in VS Code
        for i in 0..3 {
            log.record("their", "there", "VS Code", i);
        }
        // "its" → "it's" once (below threshold)
        log.record("its", "it's", "Slack", 10);

        let patterns = CorrectionPatterns::analyze(&log);
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].from_text, "their");
        assert_eq!(patterns[0].to_text, "there");
        assert_eq!(patterns[0].frequency, 3);
        assert_eq!(patterns[0].context, "VS Code");
    }

    #[test]
    fn analyze_cross_app_pattern() {
        let mut log = CorrectionLog::new();
        // Same correction in two different apps, evenly split.
        log.record("colour", "color", "VS Code", 1);
        log.record("colour", "color", "Slack", 2);

        let patterns = CorrectionPatterns::analyze(&log);
        assert_eq!(patterns.len(), 1);
        // Neither app dominates, so context should be empty.
        assert_eq!(patterns[0].context, "");
    }

    #[test]
    fn inject_hints_empty_patterns() {
        let result = inject_correction_hints("base prompt", &[]);
        assert_eq!(result, "base prompt");
    }

    #[test]
    fn inject_hints_with_patterns() {
        let patterns = vec![
            CorrectionPattern {
                from_text: "their".into(),
                to_text: "there".into(),
                frequency: 5,
                context: "VS Code".into(),
            },
            CorrectionPattern {
                from_text: "colour".into(),
                to_text: "color".into(),
                frequency: 3,
                context: String::new(),
            },
        ];

        let result = inject_correction_hints("You are helpful.", &patterns);
        assert!(result.starts_with("You are helpful."));
        assert!(result.contains("Learned user preferences"));
        assert!(result.contains("\"their\" to \"there\" in VS Code"));
        assert!(result.contains("\"colour\" to \"color\" (observed 3 times)"));
    }

    #[test]
    fn round_trip_toml() {
        let mut log = CorrectionLog::new();
        log.record("hello", "Hello", "Terminal", 42);

        let serialized = toml::to_string_pretty(&log).unwrap();
        let deserialized: CorrectionLog = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.entries().len(), 1);
        assert_eq!(deserialized.entries()[0], log.entries()[0]);
    }

    #[test]
    fn load_creates_missing_file() {
        let dir = std::env::temp_dir().join("rekody-test-corrections");
        let path = dir.join("corrections.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let log = CorrectionLog::load(&path).unwrap();
        assert!(log.entries().is_empty());
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
