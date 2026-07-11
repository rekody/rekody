//! Saved snippets and voice shortcuts.
//!
//! Maps trigger phrases to expansion text so common dictation patterns
//! expand instantly. [`expand_triggers`] runs on the final transcript
//! (after dictionary correction and optional LLM cleanup), replacing the
//! spoken form "slash sig", the literal token "/sig", or a whole-utterance
//! trigger with its stored expansion.
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

/// Check if the whole of `text` matches a trigger phrase and return its
/// expansion.
///
/// The comparison is case-insensitive. This is the whole-utterance form: the
/// user speaks nothing but the trigger. For in-sentence triggers ("send it
/// over slash sig") use [`expand_triggers`], which also handles this case.
pub fn check_and_expand(text: &str, store: &SnippetStore) -> Option<String> {
    let key = text.trim().to_lowercase();
    store.snippets.get(&key).cloned()
}

/// Expand every snippet trigger found in `text`.
///
/// Users SPEAK triggers, so STT emits words: "slash sig", not "/sig". Three
/// forms fire, all case-insensitive:
///
/// 1. The whole utterance equals a trigger (the [`check_and_expand`] form).
/// 2. The spoken form: the word "slash" followed by the trigger word(s).
/// 3. The literal token `/sig` (typed punctuation survives some engines).
///
/// The trigger must stand as its own word(s): "backslash sig" and "/sigmoid"
/// never fire. Trailing punctuation on the final trigger word is tolerated
/// and preserved, so "slash sig." expands and keeps the period. Text around
/// a trigger is left untouched; non-trigger words after "slash" stay as
/// dictated. Returns the input unchanged when the store is empty.
pub fn expand_triggers(text: &str, store: &SnippetStore) -> String {
    if store.snippets.is_empty() || text.is_empty() {
        return text.to_owned();
    }

    // Whole-utterance form.
    if let Some(expansion) = check_and_expand(text, store) {
        return expansion;
    }

    // Triggers prepared for word matching: a leading "/" on the stored
    // trigger is stripped so "/sig" and "sig" behave identically. Longest
    // (most words) first so "my email addr" wins over "my email"; ties
    // broken by trigger name for determinism.
    let mut triggers: Vec<(Vec<&str>, &str)> = store
        .snippets
        .iter()
        .filter_map(|(key, expansion)| {
            let bare = key.strip_prefix('/').unwrap_or(key);
            let words: Vec<&str> = bare.split_whitespace().collect();
            if words.is_empty() {
                None
            } else {
                Some((words, expansion.as_str()))
            }
        })
        .collect();
    triggers.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

    let words = split_words(text);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut i = 0usize;
    while i < words.len() {
        let mut matched = None;
        for (trigger_words, expansion) in &triggers {
            if let Some((consumed, end)) = match_trigger_at(text, &words, i, trigger_words) {
                matched = Some((consumed, end, *expansion));
                break;
            }
        }
        match matched {
            Some((consumed, end, expansion)) => {
                out.push_str(&text[cursor..words[i].0]);
                out.push_str(expansion);
                cursor = end;
                i += consumed;
            }
            None => i += 1,
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Try to match one trigger at word index `i`, in either the spoken form
/// ("slash" + trigger words) or the literal form ("/" + first trigger word).
///
/// Returns `(words_consumed, replacement_end_byte)`. The end byte excludes
/// trailing punctuation on the final trigger word so it survives expansion.
fn match_trigger_at(
    text: &str,
    words: &[(usize, usize)],
    i: usize,
    trigger_words: &[&str],
) -> Option<(usize, usize)> {
    let word_at = |k: usize| &text[words[k].0..words[k].1];

    // Spoken form: exactly the word "slash", then the trigger words.
    if word_at(i).eq_ignore_ascii_case("slash")
        && let Some(end) = match_trigger_words(text, words, i + 1, trigger_words, None)
    {
        return Some((1 + trigger_words.len(), end));
    }

    // Literal form: the first word is "/" glued to the first trigger word.
    if let Some(rest) = word_at(i).strip_prefix('/')
        && !rest.is_empty()
        && let Some(end) = match_trigger_words(text, words, i, trigger_words, Some(rest))
    {
        return Some((trigger_words.len(), end));
    }

    None
}

/// Match `trigger_words` against the transcript words starting at `first`.
///
/// `first_override` substitutes the text compared for the first word (used by
/// the literal form, where the leading "/" has been stripped). Inner words
/// must match exactly; the final word tolerates trailing punctuation. Returns
/// the byte offset where the replacement should end.
fn match_trigger_words(
    text: &str,
    words: &[(usize, usize)],
    first: usize,
    trigger_words: &[&str],
    first_override: Option<&str>,
) -> Option<usize> {
    if first + trigger_words.len() > words.len() {
        return None;
    }
    let mut end = 0usize;
    for (k, trigger_word) in trigger_words.iter().enumerate() {
        let (start, word_end) = words[first + k];
        let candidate = match (k, first_override) {
            (0, Some(over)) => over,
            _ => &text[start..word_end],
        };
        let is_last = k == trigger_words.len() - 1;
        if candidate.eq_ignore_ascii_case(trigger_word) {
            end = word_end;
        } else if is_last {
            // Tolerate trailing punctuation on the final trigger word only:
            // "slash sig." expands; "slash sig," does too.
            let stem = candidate.trim_end_matches(|c: char| !c.is_alphanumeric());
            if stem.is_empty() || !stem.eq_ignore_ascii_case(trigger_word) {
                return None;
            }
            end = word_end - (candidate.len() - stem.len());
        } else {
            return None;
        }
    }
    Some(end)
}

/// Byte ranges of the maximal non-whitespace runs in `text`.
fn split_words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                words.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        words.push((s, text.len()));
    }
    words
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

    // ----- expand_triggers: in-text expansion -----

    const SIG_BLOCK: &str = "Best regards,\nTony Kipkemboi\nrekody.com";

    fn sig_store() -> SnippetStore {
        let mut store = SnippetStore::new();
        store.add_snippet("sig", SIG_BLOCK);
        store
    }

    #[test]
    fn spoken_slash_form_expands_multiline_block() {
        let store = sig_store();
        assert_eq!(
            expand_triggers("send it over slash sig", &store),
            format!("send it over {SIG_BLOCK}"),
        );
        // Case-insensitive: sentence-start capitalization still fires.
        assert_eq!(expand_triggers("Slash sig", &store), SIG_BLOCK.to_string());
    }

    #[test]
    fn literal_slash_token_expands() {
        let store = sig_store();
        assert_eq!(expand_triggers("/sig", &store), SIG_BLOCK.to_string());
        assert_eq!(
            expand_triggers("here you go /sig thanks", &store),
            format!("here you go {SIG_BLOCK} thanks"),
        );
    }

    #[test]
    fn trailing_punctuation_on_trigger_is_tolerated_and_kept() {
        let store = sig_store();
        assert_eq!(
            expand_triggers("send it over slash sig.", &store),
            format!("send it over {SIG_BLOCK}."),
        );
        assert_eq!(expand_triggers("/sig,", &store), format!("{SIG_BLOCK},"),);
    }

    #[test]
    fn non_trigger_after_slash_is_untouched() {
        let store = sig_store();
        let text = "let's slash dance tonight";
        assert_eq!(expand_triggers(text, &store), text);
    }

    #[test]
    fn mid_sentence_expansion_keeps_surrounding_text() {
        let store = sig_store();
        assert_eq!(
            expand_triggers("as discussed, slash sig and see you Monday", &store),
            format!("as discussed, {SIG_BLOCK} and see you Monday"),
        );
    }

    #[test]
    fn empty_store_is_a_no_op() {
        let store = SnippetStore::new();
        let text = "send it over slash sig";
        assert_eq!(expand_triggers(text, &store), text);
    }

    #[test]
    fn trigger_must_be_its_own_word() {
        let store = sig_store();
        // "slash" embedded in another word never fires.
        assert_eq!(
            expand_triggers("backslash sig", &store),
            "backslash sig".to_string()
        );
        // The trigger embedded in a longer word never fires.
        assert_eq!(
            expand_triggers("the /sigmoid function", &store),
            "the /sigmoid function".to_string()
        );
        assert_eq!(
            expand_triggers("slash sigmoid", &store),
            "slash sigmoid".to_string()
        );
    }

    #[test]
    fn whole_utterance_trigger_still_expands() {
        let mut store = SnippetStore::new();
        store.add_snippet("my email", "tony@example.com");
        // Legacy whole-text form.
        assert_eq!(
            expand_triggers("My Email", &store),
            "tony@example.com".to_string()
        );
        // Spoken multi-word trigger mid-sentence.
        assert_eq!(
            expand_triggers("reach me at slash my email today", &store),
            "reach me at tony@example.com today".to_string()
        );
    }

    #[test]
    fn stored_trigger_with_leading_slash_matches_both_forms() {
        let mut store = SnippetStore::new();
        store.add_snippet("/addr", "123 Main St");
        assert_eq!(
            expand_triggers("ship to /addr please", &store),
            "ship to 123 Main St please".to_string()
        );
        assert_eq!(
            expand_triggers("ship to slash addr please", &store),
            "ship to 123 Main St please".to_string()
        );
    }

    #[test]
    fn multiple_triggers_expand_in_one_pass() {
        let mut store = SnippetStore::new();
        store.add_snippet("sig", "SIGNATURE");
        store.add_snippet("addr", "ADDRESS");
        assert_eq!(
            expand_triggers("slash addr then slash sig", &store),
            "ADDRESS then SIGNATURE".to_string()
        );
    }
}
