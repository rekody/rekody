//! Deterministic guard on LLM cleanup output.
//!
//! Cleanup models occasionally treat a dictation as a prompt to ANSWER
//! instead of text to clean: a question gets a reply, a request gets a
//! refusal, a test sentence gets a fabricated response. All three happened
//! with a real on-device model on 2026-07-11 (the fixtures below are those
//! transcripts). The system prompt already forbids it; small models ignore
//! that. This guard rejects output that is obviously a response rather than
//! a cleanup, and the pipeline falls back to the dictionary-corrected
//! transcript.
//!
//! The guard only runs for plain cleanup. User-selected skills legitimately
//! transform text (summarize, translate), so the caller skips it when a
//! skill is active.

/// Openers that mark a chat response. Checked against the START of the
/// cleaned output (lowercased), and forgiven when the raw dictation itself
/// starts with the same words (people do dictate "sure, let's do it").
const RESPONSE_OPENERS: [&str; 22] = [
    "i'm sorry",
    "i am sorry",
    "i apologize",
    "i can't",
    "i cannot",
    "i'm unable",
    "i am unable",
    "i'm not able",
    "as an ai",
    "as a language model",
    "sure,",
    "sure!",
    "certainly",
    "of course",
    "here's a",
    "here is a",
    // Meta-commentary about the cleanup job itself (observed from
    // llama3.2:3b: "I'll clean up your raw dictation output.").
    "i'll clean",
    "i will clean",
    "i've cleaned",
    "here's the cleaned",
    "here is the cleaned",
    "cleaned text:",
];

/// Why a cleanup output was rejected. `Display`-able for the warn log.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Rejection {
    /// Output shrank far beyond filler removal (a one-line "answer").
    Collapsed,
    /// Output grew far beyond the dictation (fabricated content).
    Inflated,
    /// Output starts like an assistant reply and the dictation does not.
    ResponseOpener,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Collapsed => write!(f, "output collapsed to a fraction of the dictation"),
            Self::Inflated => write!(f, "output ballooned past the dictation"),
            Self::ResponseOpener => write!(f, "output starts like a chat reply"),
        }
    }
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Returns `Some(reason)` when `cleaned` looks like a chat response to the
/// dictation instead of a cleanup of it, `None` when it is acceptable.
///
/// Conservative by design: cleanup legitimately removes fillers and fixes
/// punctuation, so only gross mismatches are rejected. Short dictations
/// (under 8 words) skip the length ratios — "um okay" shrinking to "Okay."
/// is exactly the job.
pub fn rejects(raw: &str, cleaned: &str) -> Option<Rejection> {
    let raw_words = word_count(raw);
    let cleaned_words = word_count(cleaned);

    if raw_words >= 8 {
        // Filler removal rarely cuts 40%; an "answer" cuts 90%+.
        if cleaned_words * 5 < raw_words * 2 {
            return Some(Rejection::Collapsed);
        }
        // Cleanup never doubles a dictation; fabrication does.
        if cleaned_words > raw_words * 2 {
            return Some(Rejection::Inflated);
        }
    }

    let cleaned_lc = cleaned.trim().to_lowercase();
    let raw_lc = raw.trim().to_lowercase();
    for opener in RESPONSE_OPENERS {
        if cleaned_lc.starts_with(opener) && !raw_lc.starts_with(opener) {
            return Some(Rejection::ResponseOpener);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // The three real failures from 2026-07-11 (Apple on-device model),
    // verbatim from history.json.

    #[test]
    fn rejects_single_word_answer() {
        let raw = "All right, so I am going to test saying the word Rakody and see \
                   if it will fix it correctly this time around. So I want to see if \
                   it's gonna pick up Recordi or Chumgay, which I already have that \
                   saved in my dictionary my snippets well not snippets but \
                   dictionary and also";
        assert_eq!(rejects(raw, "Rekody"), Some(Rejection::Collapsed));
    }

    #[test]
    fn rejects_refusal_reply() {
        let raw = "On the IOS emulator, it seems that I can scroll down this page. \
                   Everything kind of cuts off there, so I don't know if this is the \
                   correct implementation. Can you check? Specifically just this page \
                   for this scrolling effect and also confirm if that is the same \
                   issue on Android and that's fixed that";
        let cleaned = "I'm sorry, but I can't check the scrolling effect on the iOS \
                       emulator or Android device. My capabilities are limited to \
                       analyzing text and providing information based on the input I \
                       receive. If you have any specific questions or need assistance \
                       with the text you provided, feel free to ask!";
        assert_eq!(rejects(raw, cleaned), Some(Rejection::ResponseOpener));
    }

    #[test]
    fn rejects_fabricated_sample() {
        let raw = "Alright, great. So I'm going to test it to see if it's going to \
                   pick up the dictionary. For example, if it's going to pick up \
                   recording as our app name and see if it's going to catch and \
                   replace recody correctly";
        let cleaned = "Sure, let's test that. Here's a sample dictation:\n\n```\n\
                       Rekody is the best app for recording.\nI want to use it to \
                       record a video of Kipkemboi.\nChamgei the settings to 1080p.\n\
                       ```\n\nNow, let's see if the tool can correctly identify and \
                       replace \"Rekody\" with \"Recording\" and \"Chamgei\" with \
                       \"Chamgei\".";
        assert!(rejects(raw, cleaned).is_some());
    }

    // Legitimate cleanups must pass.

    #[test]
    fn rejects_meta_commentary_prefix() {
        // Real llama3.2:3b output for a normal dictation: it narrated the
        // job, then quoted the text. The opener check must catch it.
        let raw = "Alright, great. So I'm going to test it to see if it's going to \
                   pick up the dictionary. For example, if it's going to pick up \
                   recording as our app name and see if it's going to catch and \
                   replace recody correctly";
        let cleaned = "I'll clean up your raw dictation output.\n\n\"Alright, great. \
                       So I'm going to test it to see if it's going to pick up the \
                       dictionary.\"";
        assert_eq!(rejects(raw, cleaned), Some(Rejection::ResponseOpener));
    }

    #[test]
    fn accepts_filler_removal_and_punctuation() {
        let raw = "um so basically I think we should uh move the meeting to Tuesday \
                   because the client you know asked for more time";
        let cleaned =
            "I think we should move the meeting to Tuesday because the client asked for more time.";
        assert_eq!(rejects(raw, cleaned), None);
    }

    #[test]
    fn accepts_short_dictation_shrink() {
        // Under 8 words the ratios are off: shrinking "um okay" is the job.
        assert_eq!(rejects("um okay sounds good", "Okay, sounds good."), None);
    }

    #[test]
    fn accepts_dictation_that_itself_starts_like_a_reply() {
        let raw = "sure, let's move forward with the proposal we discussed and get \
                   the contract drafted by Friday for the team";
        let cleaned = "Sure, let's move forward with the proposal we discussed and \
                       get the contract drafted by Friday for the team.";
        assert_eq!(rejects(raw, cleaned), None);
    }

    #[test]
    fn accepts_unchanged_text() {
        let raw = "Send the quarterly numbers to Amara before the standup tomorrow morning please";
        assert_eq!(rejects(raw, raw), None);
    }
}
