//! Personal dictionary and custom vocabulary management.
//!
//! Allows users to maintain a list of custom terms (technical jargon, proper
//! names, domain-specific vocabulary). The dictionary is applied twice:
//!
//! 1. [`correct_text`] runs a deterministic, zero-latency correction pass on
//!    every dictation, recasing exact matches and fixing near-miss STT
//!    spellings of the user's terms. It needs no LLM, so it works with
//!    cleanup off.
//! 2. [`inject_vocabulary_prompt`] additionally lists the terms in the LLM
//!    cleanup prompt when cleanup is enabled, so the model preserves them.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

    /// Load the dictionary from the default path for read-only use on the
    /// dictation hot path. Returns an empty dictionary if the file is missing
    /// or unreadable, and never creates a file (unlike [`Dictionary::load`]).
    /// Infallible — safe to call per dictation.
    pub fn load_or_empty() -> Self {
        let Ok(path) = Self::default_path() else {
            return Self::new();
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
            .unwrap_or_default()
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

// ---------------------------------------------------------------------------
// Deterministic correction
// ---------------------------------------------------------------------------
//
// A conservative, zero-latency pass over the final transcript that fixes STT
// near-misses of dictionary terms ("recody" → "Rekody") and recases exact
// matches ("rekody" → "Rekody"). Linear scan over word tokens, no regex, no
// network — microseconds at dictation scale.

/// Cost of substituting one confusable character for another, in half-edit
/// units (see [`weighted_edit_distance`]).
const CONFUSABLE_COST: u32 = 1;
/// Cost of any other edit (insert, delete, ordinary substitution).
const EDIT_COST: u32 = 2;

/// A slice of the input text: either a word (alphanumeric run, with internal
/// apostrophes kept so "don't" stays one token) or the separator between
/// words (whitespace/punctuation, preserved byte-for-byte).
struct Token {
    start: usize,
    end: usize,
    is_word: bool,
}

/// Split `text` into word and separator tokens by byte range.
fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let is_word_char = |i: usize| -> bool {
        let c = chars[i].1;
        if c.is_alphanumeric() {
            return true;
        }
        // An apostrophe joins a word only between alphanumerics ("don't"),
        // so a possessive or quote never leaks into fuzzy matching.
        (c == '\'' || c == '\u{2019}')
            && i > 0
            && chars[i - 1].1.is_alphanumeric()
            && i + 1 < n
            && chars[i + 1].1.is_alphanumeric()
    };

    let mut tokens = Vec::new();
    let mut i = 0;
    while i < n {
        let is_word = is_word_char(i);
        let start = chars[i].0;
        let mut j = i;
        while j < n && is_word_char(j) == is_word {
            j += 1;
        }
        let end = if j < n { chars[j].0 } else { text.len() };
        tokens.push(Token {
            start,
            end,
            is_word,
        });
        i = j;
    }
    tokens
}

/// A dictionary term prepared for matching: canonical spelling plus its
/// lowercased per-word character forms.
struct PreparedTerm<'a> {
    canonical: &'a str,
    words: Vec<Vec<char>>,
}

impl<'a> PreparedTerm<'a> {
    fn prepare(raw: &'a str) -> Option<Self> {
        let canonical = raw.trim();
        if canonical.is_empty() {
            return None;
        }
        let words: Vec<Vec<char>> = canonical
            .split_whitespace()
            .map(|w| w.chars().flat_map(char::to_lowercase).collect())
            .collect();
        if words.is_empty() || words.iter().any(Vec::is_empty) {
            return None;
        }
        Some(Self { canonical, words })
    }
}

/// Whether two lowercased characters are commonly confused by STT engines:
/// any vowel for any vowel (y included) and c/k, which are the same sound.
fn confusable(a: char, b: char) -> bool {
    const VOWELS: [char; 6] = ['a', 'e', 'i', 'o', 'u', 'y'];
    (VOWELS.contains(&a) && VOWELS.contains(&b)) || matches!((a, b), ('c', 'k') | ('k', 'c'))
}

/// Levenshtein distance in half-edit units: a substitution between
/// [`confusable`] characters costs 1, every other edit costs 2. So a plain
/// edit distance of 1 is 2 units, and "recordi" → "rekody" (c→k, delete r,
/// i→y) is 4 units, i.e. two edits once the sound-alike substitutions are
/// counted as half an edit each.
fn weighted_edit_distance(a: &[char], b: &[char]) -> u32 {
    if a.is_empty() {
        return b.len() as u32 * EDIT_COST;
    }
    if b.is_empty() {
        return a.len() as u32 * EDIT_COST;
    }
    let mut prev: Vec<u32> = (0..=b.len() as u32).map(|j| j * EDIT_COST).collect();
    let mut curr = vec![0u32; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = (i as u32 + 1) * EDIT_COST;
        for (j, &cb) in b.iter().enumerate() {
            let sub_cost = if ca == cb {
                0
            } else if confusable(ca, cb) {
                CONFUSABLE_COST
            } else {
                EDIT_COST
            };
            curr[j + 1] = (prev[j] + sub_cost)
                .min(prev[j + 1] + EDIT_COST)
                .min(curr[j] + EDIT_COST);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Fuzzy tolerance for one term word, in half-edit units: one edit for terms
/// of five letters or fewer, two edits for longer terms.
fn tolerance(term_word_len: usize) -> u32 {
    if term_word_len <= 5 {
        EDIT_COST
    } else {
        2 * EDIT_COST
    }
}

/// The top 10,000 spoken-English words (OpenSubtitles frequency list,
/// vendored in `dictionary_common_words.txt`). A transcript word that IS one
/// of these is never fuzzy-rewritten into a dictionary term: people say
/// everyday words on purpose, and several sit within edit tolerance of
/// plausible user terms ("change" is two edits from a term like "Chamgei").
/// Garbled tokens ("recody") and rare real words ("recode") are not in the
/// list, so the intended corrections keep firing.
fn common_words() -> &'static HashSet<&'static str> {
    static COMMON_WORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    COMMON_WORDS.get_or_init(|| {
        include_str!("dictionary_common_words.txt")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect()
    })
}

fn is_common_word(word_lc: &[char]) -> bool {
    let word: String = word_lc.iter().collect();
    common_words().contains(word.as_str())
}

/// Whether a transcript word is a fuzzy match for one term word.
///
/// Guards, all of which must pass: first letter equal (case-insensitive),
/// length within ±2 characters, weighted distance within [`tolerance`].
fn fuzzy_word_matches(token_lc: &[char], term_word_lc: &[char]) -> bool {
    if token_lc == term_word_lc {
        return true;
    }
    if token_lc.is_empty() || term_word_lc.is_empty() || token_lc[0] != term_word_lc[0] {
        return false;
    }
    if token_lc.len().abs_diff(term_word_lc.len()) > 2 {
        return false;
    }
    weighted_edit_distance(token_lc, term_word_lc) <= tolerance(term_word_lc.len())
}

/// Find the best dictionary match starting at word index `wi`.
///
/// Returns `(span_word_count, replacement)` or `None` to leave the word
/// unchanged. Exact case-insensitive matches are tried first (longest span
/// first) and only ever recase; fuzzy candidates are then collected across
/// all span lengths and applied only when exactly ONE distinct term matches.
fn match_at<'t>(
    text: &str,
    tokens: &[Token],
    word_ix: &[usize],
    wi: usize,
    terms: &[PreparedTerm<'t>],
    max_span: usize,
) -> Option<(usize, &'t str)> {
    // Lowercased forms of the candidate span words. A span only extends
    // across whitespace-only gaps: "core, ml" is not a "Core ML" span.
    let mut span_words: Vec<Vec<char>> = Vec::new();
    for k in 0..max_span.min(word_ix.len() - wi) {
        let tok = &tokens[word_ix[wi + k]];
        if k > 0 {
            let prev = &tokens[word_ix[wi + k - 1]];
            let gap = &text[prev.end..tok.start];
            if !gap.chars().all(char::is_whitespace) {
                break;
            }
        }
        span_words.push(
            text[tok.start..tok.end]
                .chars()
                .flat_map(char::to_lowercase)
                .collect(),
        );
    }

    // Exact stage: a case-insensitive match to a term is only ever recased,
    // and shields the words from fuzzy correction into some other term.
    for len in (1..=span_words.len()).rev() {
        let mut canonicals: Vec<&str> = terms
            .iter()
            .filter(|t| t.words.len() == len && t.words == span_words[..len])
            .map(|t| t.canonical)
            .collect();
        canonicals.sort_unstable();
        canonicals.dedup();
        match canonicals.len() {
            0 => {}
            // Unique term: recase to its canonical spelling.
            1 => return Some((len, canonicals[0])),
            // Two terms differing only in case: ambiguous, leave unchanged.
            _ => return None,
        }
    }

    // Fuzzy stage: every word of the span must be within per-word tolerance
    // of the corresponding term word. Ambiguity (more than one distinct term
    // in range) means no correction. An everyday word (see [`common_words`])
    // may participate only as an exact word-level match: it is never itself
    // rewritten.
    let mut hits: Vec<&str> = Vec::new();
    for len in (1..=span_words.len()).rev() {
        for term in terms.iter().filter(|t| t.words.len() == len) {
            let all_words_match =
                term.words.iter().zip(&span_words).all(|(tw, sw)| {
                    sw == tw || (!is_common_word(sw) && fuzzy_word_matches(sw, tw))
                });
            if all_words_match {
                hits.push(term.canonical);
            }
        }
    }
    hits.sort_unstable();
    hits.dedup();
    match hits[..] {
        [only] => {
            let len = terms
                .iter()
                .find(|t| t.canonical == only)
                .map(|t| t.words.len())?;
            Some((len, only))
        }
        _ => None,
    }
}

/// Deterministically correct dictionary terms in `text`.
///
/// Word-boundary tokenization preserves all punctuation and whitespace, so
/// "rekody," becomes "Rekody," with the comma intact. Per word (and per
/// equal-length word span for multi-word terms):
///
/// - An exact case-insensitive match is recased to the term's canonical
///   spelling, and is never fuzzy-corrected into a different term.
/// - Otherwise a fuzzy match fires only when the word passes every guard
///   (see [`fuzzy_word_matches`]) for exactly ONE dictionary term, and the
///   word is not an everyday English word (see [`common_words`]): "change"
///   stays "change" even when a term like "Chamgei" is two edits away.
///
/// Runs in microseconds at dictation scale; safe to call on every transcript.
pub fn correct_text(text: &str, dictionary: &Dictionary) -> String {
    let terms: Vec<PreparedTerm<'_>> = dictionary
        .terms
        .iter()
        .filter_map(|t| PreparedTerm::prepare(t))
        .collect();
    if terms.is_empty() || text.is_empty() {
        return text.to_owned();
    }

    let tokens = tokenize(text);
    let word_ix: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_word)
        .map(|(i, _)| i)
        .collect();
    let max_span = terms.iter().map(|t| t.words.len()).max().unwrap_or(1);

    let mut out = String::with_capacity(text.len() + 16);
    let mut cursor = 0usize;
    let mut wi = 0usize;
    while wi < word_ix.len() {
        if let Some((span_len, replacement)) =
            match_at(text, &tokens, &word_ix, wi, &terms, max_span)
        {
            let start = tokens[word_ix[wi]].start;
            let end = tokens[word_ix[wi + span_len - 1]].end;
            out.push_str(&text[cursor..start]);
            out.push_str(replacement);
            cursor = end;
            wi += span_len;
        } else {
            wi += 1;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_correct_terms_pass_through_untouched() {
        let mut dict = Dictionary::new();
        dict.add_term("Rekody");
        dict.add_term("Kipkemboi");
        dict.add_term("Chamgei");
        let sample = "Rekody is the best app for recording.\n\
                      I want to use it to record a video of Kipkemboi.\n\
                      Chamgei the settings to 1080p.";
        assert_eq!(correct_text(sample, &dict), sample);
    }

    #[test]
    fn common_words_are_never_fuzzy_rewritten_into_terms() {
        let mut dict = Dictionary::new();
        dict.add_term("Chamgei");
        dict.add_term("Rekody");
        // "change" and "charge" sit within fuzzy tolerance of "Chamgei"
        // (n/m or r/m substitution plus a trailing insertion = two edits),
        // but they are everyday words: rewriting them corrupts ordinary
        // sentences, so the common-word guard must block the correction.
        assert_eq!(
            correct_text("Change the settings to 1080p.", &dict),
            "Change the settings to 1080p."
        );
        assert_eq!(
            correct_text("They charge for it.", &dict),
            "They charge for it."
        );
        // Guarded by length/distance as before, and now also by the list.
        assert_eq!(
            correct_text("record a video of the recording", &dict),
            "record a video of the recording"
        );
    }

    #[test]
    fn non_word_garbles_still_correct() {
        let mut dict = Dictionary::new();
        dict.add_term("Chamgei");
        dict.add_term("Rekody");
        assert_eq!(correct_text("open recody now", &dict), "open Rekody now");
        assert_eq!(correct_text("chamgay it", &dict), "Chamgei it");
        // "recode" is a real but uncommon word: an STT emitting it right
        // where a dictionary term fits is stronger evidence of a garble
        // than of intent, so it stays correctable.
        assert_eq!(
            correct_text("use recode please", &dict),
            "use Rekody please"
        );
    }

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

    // ----- deterministic corrector -----

    fn dict_with(terms: &[&str]) -> Dictionary {
        let mut dict = Dictionary::new();
        for t in terms {
            dict.add_term(*t);
        }
        dict
    }

    #[test]
    fn fuzzy_corrects_common_stt_misses() {
        let dict = dict_with(&["Rekody"]);
        assert_eq!(correct_text("recody", &dict), "Rekody");
        assert_eq!(correct_text("ricody", &dict), "Rekody");
        assert_eq!(correct_text("recordi", &dict), "Rekody");
        assert_eq!(
            correct_text("i installed recody yesterday", &dict),
            "i installed Rekody yesterday"
        );
    }

    #[test]
    fn exact_match_recases_only() {
        let dict = dict_with(&["Rekody"]);
        assert_eq!(correct_text("rekody", &dict), "Rekody");
        assert_eq!(correct_text("REKODY", &dict), "Rekody");
        // Already canonical: unchanged.
        assert_eq!(correct_text("Rekody", &dict), "Rekody");
    }

    #[test]
    fn real_words_are_not_corrected() {
        let dict = dict_with(&["Rekody"]);
        // "record" is 3 edits away (over threshold), "recording" fails the
        // ±2 length guard. Neither may be rewritten.
        assert_eq!(
            correct_text("record the recording", &dict),
            "record the recording"
        );
        // First-letter guard: "melody" never becomes "Rekody".
        assert_eq!(correct_text("a nice melody", &dict), "a nice melody");
    }

    #[test]
    fn exact_match_of_one_term_is_never_fuzzed_into_another() {
        // "manena" is within fuzzy range of "Maneno", but it IS the term
        // "Manena" — distance 0 to itself only recases.
        let dict = dict_with(&["Maneno", "Manena"]);
        assert_eq!(correct_text("manena", &dict), "Manena");
        assert_eq!(correct_text("maneno", &dict), "Maneno");
    }

    #[test]
    fn punctuation_stays_attached() {
        let dict = dict_with(&["Rekody"]);
        assert_eq!(correct_text("rekody,", &dict), "Rekody,");
        assert_eq!(
            correct_text("try recody, then decide.", &dict),
            "try Rekody, then decide."
        );
        assert_eq!(correct_text("(recody!)", &dict), "(Rekody!)");
    }

    #[test]
    fn multi_word_terms_match_word_spans() {
        let dict = dict_with(&["Core ML"]);
        assert_eq!(
            correct_text("we ship core ml today", &dict),
            "we ship Core ML today"
        );
        // Punctuation between the words breaks the span.
        assert_eq!(correct_text("core, ml", &dict), "core, ml");

        // Per-word tolerance: a near miss on one word of the span.
        let dict = dict_with(&["Kalenjin TTS"]);
        assert_eq!(
            correct_text("the kalenjen tts run", &dict),
            "the Kalenjin TTS run"
        );
    }

    #[test]
    fn ambiguous_between_two_terms_is_left_unchanged() {
        let dict = dict_with(&["Chamgei", "Chamgai"]);
        // "chamgi" is within tolerance of BOTH terms: leave it alone.
        assert_eq!(correct_text("open chamgi now", &dict), "open chamgi now");
        // An exact hit still recases.
        assert_eq!(correct_text("open chamgei now", &dict), "open Chamgei now");
    }

    #[test]
    fn empty_dictionary_is_a_no_op() {
        let dict = Dictionary::new();
        let text = "recody rekody record";
        assert_eq!(correct_text(text, &dict), text);
    }

    #[test]
    fn apostrophes_do_not_split_words() {
        // Guards against "don't" tokenizing as "don" + "t" and getting
        // recased by a "Don" dictionary entry.
        let dict = dict_with(&["Don"]);
        assert_eq!(correct_text("i don't know don", &dict), "i don't know Don");
    }

    #[test]
    fn whitespace_and_newlines_survive() {
        let dict = dict_with(&["Rekody"]);
        assert_eq!(
            correct_text("line one recody\nline two  rekody", &dict),
            "line one Rekody\nline two  Rekody"
        );
    }

    #[test]
    fn short_terms_get_the_tight_threshold() {
        let dict = dict_with(&["Kal"]);
        // One edit fires on a short (≤5 letter) term…
        assert_eq!(correct_text("kall", &dict), "Kal");
        // …two edits do not.
        assert_eq!(correct_text("kaale", &dict), "kaale");
        // Length guard: words more than 2 characters longer are untouched.
        assert_eq!(correct_text("kalenjin", &dict), "kalenjin");
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
