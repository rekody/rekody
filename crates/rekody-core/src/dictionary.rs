//! Personal dictionary and custom vocabulary management.
//!
//! Allows users to maintain a list of custom terms (technical jargon, proper
//! names, domain-specific vocabulary) that the LLM should recognise and
//! preserve during voice dictation cleanup.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A personal dictionary of custom vocabulary terms.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Dictionary {
    /// Custom vocabulary terms the LLM should recognise.
    #[serde(default)]
    terms: Vec<String>,
}

impl Dictionary {
    /// Create a new empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the default dictionary file path: `~/.config/rekody/dictionary.toml`.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join("rekody")
            .join("dictionary.toml"))
    }

    /// Load a dictionary from a TOML file.
    ///
    /// If the file does not exist, an empty dictionary is returned and the
    /// file is created so the user has a starting point.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!(path = %path.display(), "dictionary file not found, creating default");
            let dict = Self::new();
            dict.save(path)?;
            return Ok(dict);
        }

        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read dictionary file: {}", path.display()))?;
        let dict: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse dictionary file: {}", path.display()))?;
        tracing::info!(
            path = %path.display(),
            term_count = dict.terms.len(),
            "loaded personal dictionary"
        );
        Ok(dict)
    }

    /// Save the dictionary to a TOML file.
    ///
    /// Parent directories are created automatically if they do not exist.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create dictionary directory: {}",
                    parent.display()
                )
            })?;
        }

        let contents =
            toml::to_string_pretty(self).context("failed to serialize dictionary to TOML")?;
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write dictionary file: {}", path.display()))?;
        tracing::debug!(path = %path.display(), "dictionary saved");
        Ok(())
    }

    /// Add a term to the dictionary. Duplicates are ignored.
    pub fn add_term(&mut self, term: impl Into<String>) {
        let term = term.into();
        if !self.terms.contains(&term) {
            self.terms.push(term);
        }
    }

    /// Remove a term from the dictionary. Returns `true` if the term was present.
    pub fn remove_term(&mut self, term: &str) -> bool {
        let before = self.terms.len();
        self.terms.retain(|t| t != term);
        self.terms.len() < before
    }

    /// Return the list of custom vocabulary terms.
    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}

/// Append custom vocabulary from a [`Dictionary`] to a base LLM system prompt.
///
/// If the dictionary is empty the base prompt is returned unchanged.
/// Otherwise, a section listing the vocabulary terms is appended so the LLM
/// knows to preserve them verbatim.
pub fn inject_vocabulary_prompt(base_prompt: &str, dictionary: &Dictionary) -> String {
    if dictionary.terms.is_empty() {
        return base_prompt.to_owned();
    }

    let term_list = dictionary
        .terms
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "{base_prompt}\n\n\
         Custom vocabulary — preserve these terms exactly as written:\n\
         {term_list}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_terms() {
        let mut dict = Dictionary::new();
        dict.add_term("Rekody");
        dict.add_term("Whisper");
        dict.add_term("Rekody"); // duplicate
        assert_eq!(dict.terms().len(), 2);

        assert!(dict.remove_term("Whisper"));
        assert!(!dict.remove_term("nonexistent"));
        assert_eq!(dict.terms(), &["Rekody"]);
    }

    #[test]
    fn inject_vocabulary_empty_dictionary() {
        let dict = Dictionary::new();
        let result = inject_vocabulary_prompt("base prompt", &dict);
        assert_eq!(result, "base prompt");
    }

    #[test]
    fn inject_vocabulary_with_terms() {
        let mut dict = Dictionary::new();
        dict.add_term("Rekody");
        dict.add_term("AWTRIX");
        let result = inject_vocabulary_prompt("You are helpful.", &dict);
        assert!(result.starts_with("You are helpful."));
        assert!(result.contains("Rekody"));
        assert!(result.contains("AWTRIX"));
    }

    #[test]
    fn round_trip_toml() {
        let mut dict = Dictionary::new();
        dict.add_term("Kubernetes");
        dict.add_term("kubectl");

        let serialized = toml::to_string_pretty(&dict).unwrap();
        let deserialized: Dictionary = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.terms(), dict.terms());
    }

    #[test]
    fn load_creates_missing_file() {
        let dir = std::env::temp_dir().join("rekody-test-dict");
        let path = dir.join("dictionary.toml");
        // Clean up from any previous run.
        let _ = std::fs::remove_dir_all(&dir);

        let dict = Dictionary::load(&path).unwrap();
        assert!(dict.terms().is_empty());
        assert!(path.exists());

        // Clean up.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
