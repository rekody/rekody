//! Saved snippets and voice shortcuts.
//!
//! Maps trigger phrases to expansion text so common dictation patterns
//! can be expanded instantly — before the LLM post-processing step.
//!
//! Snippets are persisted as a TOML file at `~/.config/rekody/snippets.toml`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// On-disk representation of the snippets file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SnippetsFile {
    /// The `[[snippets]]` table array.
    #[serde(default)]
    snippets: Vec<SnippetEntry>,
}

/// A single `[[snippets]]` entry in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnippetEntry {
    /// The trigger phrase (matched case-insensitively against transcribed text).
    trigger: String,
    /// The expansion text that replaces the trigger.
    expansion: String,
}

/// In-memory store of trigger → expansion mappings.
///
/// Triggers are stored in lower-case so lookups are case-insensitive.
#[derive(Debug, Clone)]
pub struct SnippetStore {
    /// Lower-cased trigger → expansion text.
    snippets: HashMap<String, String>,
    /// Path to the backing TOML file.
    path: PathBuf,
}

impl SnippetStore {
    /// Create a new, empty [`SnippetStore`] that will persist to the default
    /// path (`~/.config/rekody/snippets.toml`).
    pub fn new() -> Self {
        Self {
            snippets: HashMap::new(),
            path: default_snippets_path(),
        }
    }

    /// Create a [`SnippetStore`] backed by an explicit file path.
    ///
    /// Useful for testing or non-standard config locations.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            snippets: HashMap::new(),
            path,
        }
    }

    // ----- persistence -----

    /// Load snippets from the backing TOML file.
    ///
    /// If the file does not exist the store is left empty (not an error).
    pub fn load(&mut self) -> Result<()> {
        let contents = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %self.path.display(), "snippets file not found, starting empty");
                return Ok(());
            }
            Err(e) => {
                return Err(e).context(format!(
                    "failed to read snippets file at {}",
                    self.path.display()
                ));
            }
        };

        let file: SnippetsFile = toml::from_str(&contents).context(format!(
            "failed to parse snippets file at {}",
            self.path.display()
        ))?;

        self.snippets.clear();
        for entry in file.snippets {
            self.snippets
                .insert(entry.trigger.to_lowercase(), entry.expansion);
        }

        tracing::info!(count = self.snippets.len(), "snippets loaded");
        Ok(())
    }

    /// Save the current snippets to the backing TOML file.
    ///
    /// Parent directories are created automatically.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).context(format!(
                "failed to create config directory {}",
                parent.display()
            ))?;
        }

        let entries: Vec<SnippetEntry> = self
            .snippets
            .iter()
            .map(|(trigger, expansion)| SnippetEntry {
                trigger: trigger.clone(),
                expansion: expansion.clone(),
            })
            .collect();

        let file = SnippetsFile { snippets: entries };
        let toml_string =
            toml::to_string_pretty(&file).context("failed to serialize snippets to TOML")?;

        std::fs::write(&self.path, toml_string).context(format!(
            "failed to write snippets file at {}",
            self.path.display()
        ))?;

        tracing::info!(path = %self.path.display(), "snippets saved");
        Ok(())
    }

    // ----- mutations -----

    /// Add or update a snippet mapping.
    ///
    /// The trigger is stored lower-cased for case-insensitive matching.
    pub fn add_snippet(&mut self, trigger: &str, expansion: &str) {
        self.snippets
            .insert(trigger.to_lowercase(), expansion.to_string());
    }

    /// Remove a snippet by its trigger phrase (case-insensitive).
    ///
    /// Returns `true` if the snippet existed and was removed.
    pub fn remove_snippet(&mut self, trigger: &str) -> bool {
        self.snippets.remove(&trigger.to_lowercase()).is_some()
    }

    // ----- queries -----

    /// List all stored snippets as `(trigger, expansion)` pairs.
    pub fn list(&self) -> Vec<(&str, &str)> {
        self.snippets
            .iter()
            .map(|(t, e)| (t.as_str(), e.as_str()))
            .collect()
    }
}

impl Default for SnippetStore {
    fn default() -> Self {
        Self::new()
    }
}

// ----- free function -----

/// Check if `text` matches a trigger phrase and return its expansion.
///
/// The comparison is case-insensitive. This is intended to run **before** the
/// LLM post-processing step for instant expansion.
pub fn check_and_expand(text: &str, store: &SnippetStore) -> Option<String> {
    let key = text.trim().to_lowercase();
    store.snippets.get(&key).cloned()
}

/// Return the default snippets file path: `~/.config/rekody/snippets.toml`.
fn default_snippets_path() -> PathBuf {
    std::env::var("HOME")
        .map(|h| {
            PathBuf::from(h)
                .join(".config")
                .join("rekody")
                .join("snippets.toml")
        })
        .unwrap_or_else(|_| PathBuf::from("snippets.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn add_and_expand() {
        let mut store = SnippetStore::new();
        store.add_snippet("my email", "tony@example.com");

        assert_eq!(
            check_and_expand("my email", &store),
            Some("tony@example.com".to_string()),
        );
        // Case-insensitive.
        assert_eq!(
            check_and_expand("My Email", &store),
            Some("tony@example.com".to_string()),
        );
        // No match.
        assert_eq!(check_and_expand("something else", &store), None);
    }

    #[test]
    fn remove_snippet() {
        let mut store = SnippetStore::new();
        store.add_snippet("sig", "Best regards, Tony");
        assert!(store.remove_snippet("SIG"));
        assert_eq!(check_and_expand("sig", &store), None);
        // Removing again returns false.
        assert!(!store.remove_snippet("sig"));
    }

    #[test]
    fn list_snippets() {
        let mut store = SnippetStore::new();
        store.add_snippet("a", "alpha");
        store.add_snippet("b", "bravo");
        let mut items = store.list();
        items.sort();
        assert_eq!(items, vec![("a", "alpha"), ("b", "bravo")]);
    }

    #[test]
    fn load_from_toml() {
        let toml_content = r#"
[[snippets]]
trigger = "my addr"
expansion = "123 Main St"

[[snippets]]
trigger = "signoff"
expansion = "Best regards,\nTony"
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(toml_content.as_bytes()).unwrap();

        let mut store = SnippetStore::with_path(tmp.path().to_path_buf());
        store.load().unwrap();

        assert_eq!(
            check_and_expand("my addr", &store),
            Some("123 Main St".to_string()),
        );
        assert_eq!(
            check_and_expand("SIGNOFF", &store),
            Some("Best regards,\nTony".to_string()),
        );
    }

    #[test]
    fn save_and_reload() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let mut store = SnippetStore::with_path(path.clone());
        store.add_snippet("greet", "Hello there!");
        store.save().unwrap();

        let mut store2 = SnippetStore::with_path(path);
        store2.load().unwrap();
        assert_eq!(
            check_and_expand("greet", &store2),
            Some("Hello there!".to_string()),
        );
    }

    #[test]
    fn load_missing_file_is_ok() {
        let mut store = SnippetStore::with_path(PathBuf::from("/tmp/does_not_exist_rekody.toml"));
        assert!(store.load().is_ok());
        assert!(store.list().is_empty());
    }

    #[test]
    fn whitespace_trimmed_on_lookup() {
        let mut store = SnippetStore::new();
        store.add_snippet("hello", "world");
        assert_eq!(
            check_and_expand("  hello  ", &store),
            Some("world".to_string()),
        );
    }
}
