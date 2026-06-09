//! Deterministic number / currency / percent / unit normalization.
//!
//! A conservative final pass over dictation output that converts spoken-form
//! quantities into conventional written form — the kind of formatting an LLM
//! does inconsistently (Deepgram's `smart_format` does it deterministically).
//!
//! Examples:
//! - "three hundred fifty"        → "350"
//! - "two thousand twenty six"    → "2026"
//! - "fifty dollars"              → "$50"
//! - "twenty percent"             → "20%"
//! - "five kilograms"             → "5 kg"
//!
//! Deliberately CONSERVATIVE to avoid false positives:
//! - Isolated small number words (`one`..`nine`) are LEFT as words (matching
//!   the common style of spelling out single digits) UNLESS attached to a
//!   currency/percent/unit, where digits are expected ("five dollars" → "$5").
//! - Only converts a run that forms a valid number; non-number words pass
//!   through byte-for-byte. Processed per line, so newlines are preserved.
//! - Only a curated set of UNAMBIGUOUS units is converted (kilograms → kg);
//!   ambiguous words (pounds, feet, minutes) are left alone.

/// Additive number word → value (0–90).
fn small(word: &str) -> Option<u64> {
    Some(match word {
        "zero" => 0,
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "thirteen" => 13,
        "fourteen" => 14,
        "fifteen" => 15,
        "sixteen" => 16,
        "seventeen" => 17,
        "eighteen" => 18,
        "nineteen" => 19,
        "twenty" => 20,
        "thirty" => 30,
        "forty" => 40,
        "fifty" => 50,
        "sixty" => 60,
        "seventy" => 70,
        "eighty" => 80,
        "ninety" => 90,
        _ => return None,
    })
}

/// Multiplicative scale word → multiplier (1000+). `hundred` handled separately.
fn scale(word: &str) -> Option<u64> {
    Some(match word {
        "thousand" => 1_000,
        "million" => 1_000_000,
        "billion" => 1_000_000_000,
        _ => return None,
    })
}

/// Is `word` any token that can appear inside a number run?
fn is_number_word(word: &str) -> bool {
    small(word).is_some() || scale(word).is_some() || word == "hundred"
}

/// Curated UNAMBIGUOUS units: spoken word → symbol. Ambiguous units omitted.
fn unit_symbol(word: &str) -> Option<&'static str> {
    Some(match word {
        "kilograms" | "kilogram" => "kg",
        "kilometers" | "kilometer" | "kilometres" | "kilometre" => "km",
        "milligrams" | "milligram" => "mg",
        "milliliters" | "milliliter" | "millilitres" | "millilitre" => "ml",
        "megabytes" | "megabyte" => "MB",
        "gigabytes" | "gigabyte" => "GB",
        "kilobytes" | "kilobyte" => "KB",
        "terabytes" | "terabyte" => "TB",
        "megahertz" => "MHz",
        "gigahertz" => "GHz",
        _ => return None,
    })
}

/// Combine a run of number words into a single integer.
fn words_to_number(words: &[String]) -> u64 {
    let mut result: u64 = 0;
    let mut current: u64 = 0;
    for w in words {
        let w = w.as_str();
        if let Some(n) = small(w) {
            current += n;
        } else if w == "hundred" {
            current = current.max(1) * 100;
        } else if let Some(s) = scale(w) {
            result += current.max(1) * s;
            current = 0;
        }
        // "and" and anything else inside a run is ignored.
    }
    result + current
}

/// Is this run a well-formed cardinal number? Rejects malformed sequences like
/// "twenty twenty six" (a year said in pairs) or "five six", which should be
/// left as words rather than mis-summed. The only valid adjacency between two
/// additive ("small") words is tens (20,30,…90) followed by a unit (1–9).
fn is_valid_cardinal(words: &[String]) -> bool {
    let mut prev_small: Option<u64> = None;
    for w in words {
        let w = w.as_str();
        if let Some(v) = small(w) {
            if let Some(p) = prev_small {
                let ok = p >= 20 && p % 10 == 0 && (1..=9).contains(&v);
                if !ok {
                    return false;
                }
            }
            prev_small = Some(v);
        } else {
            // hundred / thousand / million / billion resets the additive group.
            prev_small = None;
        }
    }
    true
}

/// Split trailing punctuation off a token: ("percent.", ) → ("percent", ".").
fn split_trailing_punct(tok: &str) -> (&str, &str) {
    let end = tok
        .rfind(|c: char| !".,;:!?)\"'".contains(c))
        .map(|i| i + tok[i..].chars().next().unwrap().len_utf8())
        .unwrap_or(0);
    tok.split_at(end)
}

/// Normalize numbers/currency/percent/units across the whole text.
/// Processed line-by-line so newlines are preserved.
pub fn normalize(text: &str) -> String {
    text.split('\n')
        .map(normalize_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_line(line: &str) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        // Lower-cased, punctuation-stripped core for matching.
        let (core, _) = split_trailing_punct(tokens[i]);
        let core_lc = core.to_lowercase();

        // Try to start a number run.
        if is_number_word(&core_lc) {
            // Collect the maximal run of number words (allowing "and" between).
            let start = i;
            let mut run_words: Vec<String> = Vec::new();
            let mut last_was_number = false;
            while i < tokens.len() {
                let (c, _) = split_trailing_punct(tokens[i]);
                let clc = c.to_lowercase();
                if is_number_word(&clc) {
                    run_words.push(clc);
                    last_was_number = true;
                    i += 1;
                } else if clc == "and" && last_was_number && i + 1 < tokens.len() && {
                    let (nc, _) = split_trailing_punct(tokens[i + 1]);
                    is_number_word(&nc.to_lowercase())
                } {
                    // "three hundred and fifty" — skip the joining "and".
                    last_was_number = false;
                    i += 1;
                } else {
                    break;
                }
            }

            let word_count = run_words.len();

            // Bail on malformed runs (e.g. "twenty twenty six") — leave as words.
            if !is_valid_cardinal(&run_words) {
                for t in &tokens[start..i] {
                    out.push((*t).to_string());
                }
                continue;
            }

            let value = words_to_number(&run_words);
            // Preserve any trailing punctuation from the run's last real token.
            let (_, run_punct) = split_trailing_punct(tokens[i - 1]);

            // Peek the following token for a currency/percent/unit suffix.
            let mut suffix: Option<String> = None; // formatted replacement token
            let mut consume_suffix = false;
            let mut force_digits = false;
            if i < tokens.len() {
                let (sc, sp) = split_trailing_punct(tokens[i]);
                match sc.to_lowercase().as_str() {
                    "percent" => {
                        suffix = Some(format!("{value}%{sp}"));
                        consume_suffix = true;
                        force_digits = true;
                    }
                    "dollars" | "dollar" => {
                        suffix = Some(format!("${value}{sp}"));
                        consume_suffix = true;
                        force_digits = true;
                    }
                    other => {
                        if let Some(sym) = unit_symbol(other) {
                            suffix = Some(format!("{value} {sym}{sp}"));
                            consume_suffix = true;
                            force_digits = true;
                        }
                    }
                }
            }

            // Decide whether to emit digits at all.
            let emit_digits = force_digits || word_count >= 2 || value >= 10;

            if !emit_digits {
                // Isolated small number word — leave the original token(s) intact.
                for t in &tokens[start..i] {
                    out.push((*t).to_string());
                }
                continue;
            }

            if consume_suffix {
                out.push(suffix.unwrap());
                i += 1; // consume the suffix token
            } else {
                out.push(format!("{value}{run_punct}"));
            }
            continue;
        }

        // Existing digit token followed by a "percent"/"dollars"/unit word —
        // catches LLM output that digitized the number but spelled the suffix.
        if let Some(formatted) = digit_with_suffix(&tokens, &mut i) {
            out.push(formatted);
            continue;
        }

        out.push(tokens[i].to_string());
        i += 1;
    }

    out.join(" ")
}

/// If tokens[i] is a bare integer and tokens[i+1] is percent/dollars/unit,
/// format and advance `i` past both. Returns the formatted token.
fn digit_with_suffix(tokens: &[&str], i: &mut usize) -> Option<String> {
    let cur = tokens[*i];
    if cur.is_empty() || !cur.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if *i + 1 >= tokens.len() {
        return None;
    }
    let (sc, sp) = split_trailing_punct(tokens[*i + 1]);
    let formatted = match sc.to_lowercase().as_str() {
        "percent" => format!("{cur}%{sp}"),
        "dollars" | "dollar" => format!("${cur}{sp}"),
        other => {
            let sym = unit_symbol(other)?;
            format!("{cur} {sym}{sp}")
        }
    };
    *i += 2;
    Some(formatted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_numbers() {
        assert_eq!(normalize("three hundred fifty"), "350");
        assert_eq!(normalize("two thousand twenty six"), "2026");
        assert_eq!(normalize("twenty five"), "25");
        assert_eq!(normalize("one hundred and one"), "101");
        assert_eq!(normalize("fifty thousand"), "50000");
    }

    #[test]
    fn isolated_small_numbers_stay_words() {
        // Single digits <10 spelled out (style) and avoid false positives.
        assert_eq!(normalize("I have one idea"), "I have one idea");
        assert_eq!(normalize("give me a second"), "give me a second");
        assert_eq!(normalize("just three things"), "just three things");
        // 10+ converts.
        assert_eq!(normalize("about twenty people"), "about 20 people");
    }

    #[test]
    fn currency_forces_digits_and_symbol() {
        assert_eq!(normalize("it costs fifty dollars"), "it costs $50");
        assert_eq!(normalize("five dollars please"), "$5 please");
        assert_eq!(normalize("two hundred dollars"), "$200");
    }

    #[test]
    fn percent_forces_digits_and_symbol() {
        assert_eq!(normalize("up twenty percent"), "up 20%");
        assert_eq!(normalize("five percent growth"), "5% growth");
    }

    #[test]
    fn units_when_unambiguous() {
        assert_eq!(normalize("five kilograms"), "5 kg");
        assert_eq!(normalize("two hundred megabytes"), "200 MB");
        // Ambiguous unit left alone.
        assert_eq!(normalize("five pounds"), "five pounds");
    }

    #[test]
    fn digit_then_spelled_suffix() {
        assert_eq!(normalize("50 percent"), "50%");
        assert_eq!(normalize("20 dollars"), "$20");
        assert_eq!(normalize("3 kilograms"), "3 kg");
    }

    #[test]
    fn preserves_punctuation_and_newlines() {
        assert_eq!(normalize("we grew fifty percent."), "we grew 50%.");
        assert_eq!(
            normalize("line one\ntwenty five things"),
            "line one\n25 things"
        );
        assert_eq!(normalize("really, twenty dollars?"), "really, $20?");
    }

    #[test]
    fn malformed_runs_left_as_words() {
        // Year said in pairs — ambiguous, must NOT become "46".
        assert_eq!(
            normalize("see you in twenty twenty six"),
            "see you in twenty twenty six"
        );
        assert_eq!(
            normalize("back in nineteen eighty four"),
            "back in nineteen eighty four"
        );
        // But the unambiguous full form still converts.
        assert_eq!(
            normalize("the year two thousand twenty six"),
            "the year 2026"
        );
    }

    #[test]
    fn non_numbers_pass_through() {
        assert_eq!(normalize("hello world"), "hello world");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("the quick brown fox"), "the quick brown fox");
    }
}
