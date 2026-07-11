//! Normalization, word error rate, and the triage buckets the review UI
//! groups clips by.
//!
//! This WER is a triage signal, not a benchmark number: both sides are
//! folded to lowercase with punctuation stripped before comparison, so pure
//! formatting differences ("OK," vs "ok") never open a diff. The reference
//! side is the manifest label, matching the sidecar's "label vs teacher"
//! framing.

/// Lowercase `text` and strip punctuation, collapsing whitespace runs.
/// Only alphanumeric characters survive inside tokens, so "Don't!" and
/// "dont" normalize identically.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        let cleaned: String = word
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if !cleaned.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&cleaned);
        }
    }
    out
}

/// Normalized word tokens of `text`.
fn norm_tokens(text: &str) -> Vec<String> {
    normalize(text)
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// Word error rate of the manifest label against the teacher transcript:
/// word-level Levenshtein distance over normalized tokens, divided by the
/// label's token count. 0.0 means the models agree exactly after
/// normalization; an empty label against a non-empty teacher scores 1.0.
pub fn wer(label: &str, teacher: &str) -> f64 {
    let a = norm_tokens(label);
    let b = norm_tokens(teacher);
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    if a.is_empty() {
        return 1.0;
    }
    levenshtein_words(&a, &b) as f64 / a.len() as f64
}

/// Classic two-row Levenshtein over word tokens.
fn levenshtein_words(a: &[String], b: &[String]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, wa) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, wb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(wa != wb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Triage group for a scored clip. Thresholds mirror the UI copy: anything
/// above 0.15 needs a listen, small non-zero disagreement is a quick diff,
/// exact agreement means the label already matches the teacher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// wer > 0.15: the two models disagree enough that ears are required.
    ListenFirst,
    /// 0 < wer <= 0.15: a handful of words differ; the diff usually decides.
    QuickDiff,
    /// wer == 0: label and teacher agree post-normalization.
    Agree,
}

/// Bucket a WER value into its triage group.
pub fn bucket(wer: f64) -> Bucket {
    if wer > 0.15 {
        Bucket::ListenFirst
    } else if wer > 0.0 {
        Bucket::QuickDiff
    } else {
        Bucket::Agree
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_case_and_punctuation() {
        assert_eq!(normalize("Hello,  World!"), "hello world");
        assert_eq!(normalize("Don't stop."), "dont stop");
        assert_eq!(normalize("  ...  "), "");
        // Punctuation-only tokens vanish instead of leaving empty words.
        assert_eq!(normalize("yes - no"), "yes no");
    }

    #[test]
    fn wer_ignores_formatting_differences() {
        assert_eq!(wer("OK, sounds good!", "ok sounds good"), 0.0);
        assert_eq!(wer("", ""), 0.0);
    }

    #[test]
    fn wer_counts_word_edits_against_label_length() {
        // One substitution in ten words.
        let label = "one two three four five six seven eight nine ten";
        let teacher = "one two three four five six seven eight nine zen";
        assert!((wer(label, teacher) - 0.1).abs() < 1e-9);

        // One insertion against a four-word label.
        assert!((wer("a b c d", "a b x c d") - 0.25).abs() < 1e-9);

        // Empty label vs non-empty teacher pegs at 1.0.
        assert_eq!(wer("", "something was said"), 1.0);

        // Total disagreement is >= 1.0 worth of edits.
        assert!(wer("alpha beta", "gamma delta epsilon") >= 1.0);
    }

    #[test]
    fn buckets_match_ui_thresholds() {
        assert_eq!(bucket(0.0), Bucket::Agree);
        assert_eq!(bucket(0.05), Bucket::QuickDiff);
        assert_eq!(bucket(0.15), Bucket::QuickDiff); // boundary stays quick
        assert_eq!(bucket(0.16), Bucket::ListenFirst);
        assert_eq!(bucket(2.0), Bucket::ListenFirst);
    }
}
