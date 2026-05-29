//! User-defined "skills" — reusable LLM cleanup/transform presets.
//!
//! A skill is a Markdown file with a small YAML-style frontmatter block and a
//! body that IS the system prompt sent to the LLM for post-processing. Skills
//! let a user reshape raw dictation into a specific form (email, notes, spec,
//! commit message, …) on demand, instead of relying only on the built-in
//! app-context prompts.
//!
//! ```text
//! ---
//! name: email
//! description: Professional email — greeting, body, sign-off
//! triggers: Mail, Spark, Superhuman      # optional app auto-apply
//! inherit_base: false                     # optional; prepend the strict cleanup rules
//! ---
//! You turn a raw voice transcription into a professional email.
//! - ...
//! ```
//!
//! Skills live in `~/.config/rekody/skills/*.md`. The *currently active* skill
//! (a sticky selection that persists across dictations) is stored separately in
//! `~/.config/rekody/skill.toml` so it never touches the hand-editable
//! `config.toml`.
//!
//! ## Prompt precedence (see [`resolve`])
//! 1. An explicitly selected ("sticky") skill wins.
//! 2. Otherwise, a skill whose `triggers` match the focused app applies.
//! 3. Otherwise → `None`, and the caller falls back to the built-in
//!    app-context prompt ([`crate::prompts::get_prompt_for_app`]).
//!
//! The skill body REPLACES the built-in prompt rather than appending to it —
//! the built-in [`crate::prompts::BASE_PROMPT`] forbids reformatting/expansion,
//! which directly conflicts with skills like "turn this into an email". Set
//! `inherit_base: true` in frontmatter to opt back into those strict rules.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Always appended to a skill prompt: model output is pasted directly into the
/// focused app, so stray markdown fences or quotes would land in the document.
const OUTPUT_HYGIENE: &str = "\n\nOUTPUT RULES: Return only the finished text — \
no preamble, no explanation, no surrounding quotes, and no markdown code fences. \
Never ask follow-up questions.";

/// Starter skills embedded in the binary, seeded into the user's skills
/// directory on first use. `(file_stem, file_contents)`.
const STARTER_SKILLS: &[(&str, &str)] = &[
    ("email", include_str!("../assets/skills/email.md")),
    ("notes", include_str!("../assets/skills/notes.md")),
    ("spec", include_str!("../assets/skills/spec.md")),
    ("slack", include_str!("../assets/skills/slack.md")),
    ("summary", include_str!("../assets/skills/summary.md")),
    ("todo", include_str!("../assets/skills/todo.md")),
    ("journal", include_str!("../assets/skills/journal.md")),
    ("commit", include_str!("../assets/skills/commit.md")),
];

/// A parsed skill.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Skill name (from frontmatter `name`, else the file stem).
    pub name: String,
    /// One-line description for pickers/lists.
    pub description: String,
    /// App-name / bundle-id substrings that auto-apply this skill (lower-cased).
    pub triggers: Vec<String>,
    /// If true, prepend [`crate::prompts::BASE_PROMPT`] (strict cleanup rules)
    /// before the body. Default false (skills fully define their own behavior).
    pub inherit_base: bool,
    /// The system prompt body.
    pub body: String,
}

impl Skill {
    /// Compose the full system prompt this skill sends to the LLM.
    pub fn system_prompt(&self) -> String {
        let mut p = String::new();
        if self.inherit_base {
            p.push_str(crate::prompts::BASE_PROMPT);
            p.push_str("\n\n");
        }
        p.push_str(self.body.trim());
        p.push_str(OUTPUT_HYGIENE);
        p
    }

    /// Does this skill's trigger list match the focused app?
    fn matches_app(&self, app_name: &str, bundle_id: Option<&str>) -> bool {
        if self.triggers.is_empty() {
            return false;
        }
        let name = app_name.to_lowercase();
        let bid = bundle_id.map(|b| b.to_lowercase());
        self.triggers
            .iter()
            .any(|t| name.contains(t) || bid.as_deref().is_some_and(|b| b.contains(t)))
    }
}

// ── On-disk active-skill state (~/.config/rekody/skill.toml) ─────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillState {
    /// Name of the sticky active skill, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<String>,
}

// ── Paths ────────────────────────────────────────────────────────────────────

/// `~/.config/rekody`. Returns `None` when `$HOME` is unset, matching the
/// behavior in history.rs / stats.rs — we never fall back to the CWD, so a
/// stray `./skills` directory can't be picked up in an unusual launch env.
fn config_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config").join("rekody"))
}

/// `~/.config/rekody/skills` — the directory of `*.md` skill files.
pub fn skills_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("skills"))
}

/// `~/.config/rekody/skill.toml` — the sticky active-skill selection.
fn state_path() -> Option<PathBuf> {
    Some(config_dir()?.join("skill.toml"))
}

// ── Frontmatter parsing ────────────────────────────────────────────────────

/// Parse a skill from a Markdown file's contents.
///
/// `stem` is the filename without extension, used as the name when frontmatter
/// omits one. Parsing is forgiving: a file with no frontmatter is treated as a
/// pure prompt body.
fn parse_skill(stem: &str, contents: &str) -> Skill {
    let mut name = stem.to_string();
    let mut description = String::new();
    let mut triggers = Vec::new();
    let mut inherit_base = false;

    // Frontmatter is the block between a leading `---` line and the next `---`.
    let body = if let Some(rest) = contents.strip_prefix("---") {
        // Only treat it as frontmatter if the first line really is just `---`.
        let rest = rest
            .strip_prefix('\n')
            .or_else(|| rest.strip_prefix("\r\n"));
        if let Some(rest) = rest {
            if let Some(end) = find_frontmatter_end(rest) {
                let (front, after) = rest.split_at(end.0);
                for line in front.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Some((key, value)) = line.split_once(':') else {
                        continue;
                    };
                    let key = key.trim().to_lowercase();
                    let value = value.trim();
                    match key.as_str() {
                        "name" => name = strip_quotes(value).to_string(),
                        "description" => description = strip_quotes(value).to_string(),
                        "triggers" => triggers = parse_triggers(value),
                        "inherit_base" => {
                            inherit_base = matches!(value.to_lowercase().as_str(), "true" | "yes")
                        }
                        _ => {}
                    }
                }
                // `after` starts at the closing `---`; skip past that line.
                after[end.1..].to_string()
            } else {
                contents.to_string()
            }
        } else {
            contents.to_string()
        }
    } else {
        contents.to_string()
    };

    Skill {
        name,
        description,
        triggers,
        inherit_base,
        body: body.trim().to_string(),
    }
}

/// Find the closing `---` line in `rest` (the text after the opening fence).
/// Returns `(offset_of_closing_fence, length_consumed_including_newline)`.
fn find_frontmatter_end(rest: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.trim() == "---" {
            return Some((offset, line.len()));
        }
        offset += line.len();
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    s.trim()
        .trim_start_matches(['"', '\''])
        .trim_end_matches(['"', '\''])
}

/// Parse the `triggers` value: comma-separated, tolerant of `[a, b]` form.
/// Returns lower-cased trigger substrings.
fn parse_triggers(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|t| strip_quotes(t).trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

// ── Store operations ─────────────────────────────────────────────────────────

/// Write the embedded starter skills into the skills directory if it does not
/// yet contain any `*.md` files. Idempotent; never overwrites user files.
pub fn ensure_starter_pack() -> Result<()> {
    let Some(dir) = skills_dir() else {
        return Ok(());
    };
    let has_skills = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        })
        .unwrap_or(false);
    if has_skills {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating skills dir {}", dir.display()))?;
    for (stem, contents) in STARTER_SKILLS {
        let path = dir.join(format!("{stem}.md"));
        if !path.exists() {
            std::fs::write(&path, contents)
                .with_context(|| format!("writing starter skill {}", path.display()))?;
        }
    }
    tracing::info!(dir = %dir.display(), count = STARTER_SKILLS.len(), "seeded starter skills");
    Ok(())
}

/// List all skills currently on disk, sorted by name. Read-only (does not seed).
/// Returns an empty vec if the directory is missing or unreadable.
pub fn list_skills() -> Vec<Skill> {
    match skills_dir() {
        Some(dir) => list_skills_in(&dir),
        None => Vec::new(),
    }
}

/// List skills in an explicit directory (used by tests).
fn list_skills_in(dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => skills.push(parse_skill(stem, &contents)),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping unreadable skill")
            }
        }
    }
    skills.sort_by_key(|s| s.name.to_lowercase());
    skills
}

/// Load a single skill by name (case-insensitive). `None` if not found.
pub fn load_skill(name: &str) -> Option<Skill> {
    let target = name.to_lowercase();
    list_skills()
        .into_iter()
        .find(|s| s.name.to_lowercase() == target)
}

/// The name of the currently active sticky skill, if any.
pub fn active_name() -> Option<String> {
    let contents = std::fs::read_to_string(state_path()?).ok()?;
    let state: SkillState = toml::from_str(&contents).ok()?;
    state.active
}

/// Set (or with `None`, clear) the sticky active skill and persist it.
pub fn set_active(name: Option<&str>) -> Result<()> {
    let path = state_path().context("could not determine config dir ($HOME unset)")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir {}", parent.display()))?;
    }
    let state = SkillState {
        active: name.map(|s| s.to_string()),
    };
    let toml_string = toml::to_string_pretty(&state).context("serializing skill state")?;
    std::fs::write(&path, toml_string).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The skill chosen for a dictation, plus its composed system prompt.
#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    /// The skill's name (for surfacing in the status line / logs).
    pub name: String,
    /// The composed system prompt to send to the LLM.
    pub prompt: String,
}

/// Resolve the skill for the current dictation, honoring (1) an explicit
/// sticky skill, then (2) a skill whose triggers match the focused app.
/// Returns `None` to defer to the built-in app-context prompt.
///
/// Read-only and infallible — safe to call on the dictation hot path.
pub fn resolve(app_name: &str, bundle_id: Option<&str>) -> Option<ResolvedSkill> {
    resolve_in(
        &list_skills(),
        active_name().as_deref(),
        app_name,
        bundle_id,
    )
}

/// Core precedence logic, operating on an explicit skill set + active name so
/// it can be unit-tested without touching disk or `$HOME`.
///
/// A skill with an empty body and `inherit_base = false` is skipped — it would
/// otherwise replace the strong built-in prompt with nothing but the output
/// hygiene tail, degrading behavior. Such a skill falls through to the next
/// candidate (or to `None`).
fn resolve_in(
    skills: &[Skill],
    active: Option<&str>,
    app_name: &str,
    bundle_id: Option<&str>,
) -> Option<ResolvedSkill> {
    let usable = |s: &Skill| !s.body.trim().is_empty() || s.inherit_base;

    // 1. Explicit sticky selection wins.
    if let Some(active) = active {
        match skills.iter().find(|s| s.name.eq_ignore_ascii_case(active)) {
            Some(skill) if usable(skill) => {
                return Some(ResolvedSkill {
                    name: skill.name.clone(),
                    prompt: skill.system_prompt(),
                });
            }
            Some(_) => tracing::warn!(skill = %active, "active skill has empty body; ignoring"),
            None => tracing::warn!(skill = %active, "active skill not found on disk; ignoring"),
        }
    }
    // 2. App-trigger auto-apply.
    skills
        .iter()
        .filter(|s| usable(s))
        .find(|s| s.matches_app(app_name, bundle_id))
        .map(|s| ResolvedSkill {
            name: s.name.clone(),
            prompt: s.system_prompt(),
        })
}

/// Advance the sticky active skill to the next one in the rotation
/// `[Auto, <skills sorted by name>]`, wrapping around, and persist it.
/// Returns the new selection (`None` = Auto). Used by the ⌥Space+Tab hotkey.
pub fn cycle_active() -> Option<String> {
    let skills = list_skills();
    let next = cycle_next(&skills, active_name().as_deref());
    let _ = set_active(next.as_deref());
    next
}

/// Pure cycle logic (testable): given the available skills and the current
/// active name, return the next selection. Order: Auto → first → … → last →
/// Auto. An unknown/missing current is treated as Auto (→ first skill).
fn cycle_next(skills: &[Skill], current: Option<&str>) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    match current {
        None => Some(skills[0].name.clone()),
        Some(cur) => match skills.iter().position(|s| s.name.eq_ignore_ascii_case(cur)) {
            Some(i) if i + 1 < skills.len() => Some(skills[i + 1].name.clone()),
            Some(_) => None, // was the last skill → wrap to Auto
            None => Some(skills[0].name.clone()), // unknown current → first
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let src = "---\nname: email\ndescription: Pro email\ntriggers: Mail, Spark\ninherit_base: false\n---\nYou turn dictation into email.\n- be nice\n";
        let s = parse_skill("fallback", src);
        assert_eq!(s.name, "email");
        assert_eq!(s.description, "Pro email");
        assert_eq!(s.triggers, vec!["mail", "spark"]);
        assert!(!s.inherit_base);
        assert!(s.body.starts_with("You turn dictation into email."));
        assert!(!s.body.contains("---"));
    }

    #[test]
    fn no_frontmatter_is_pure_body() {
        let s = parse_skill("raw", "Just a prompt body.");
        assert_eq!(s.name, "raw");
        assert_eq!(s.body, "Just a prompt body.");
        assert!(s.triggers.is_empty());
    }

    #[test]
    fn triggers_tolerate_bracket_form() {
        assert_eq!(parse_triggers("[Mail, \"Spark\"]"), vec!["mail", "spark"]);
        assert_eq!(parse_triggers("Slack"), vec!["slack"]);
        assert!(parse_triggers("").is_empty());
    }

    #[test]
    fn system_prompt_replace_vs_inherit() {
        let replace = parse_skill("x", "BODY");
        let p = replace.system_prompt();
        assert!(p.starts_with("BODY"));
        assert!(!p.contains("strict voice dictation cleanup tool"));
        assert!(p.contains("OUTPUT RULES"));

        let inherit = parse_skill("y", "---\ninherit_base: true\n---\nBODY");
        let p2 = inherit.system_prompt();
        assert!(p2.contains("strict voice dictation cleanup tool"));
        assert!(p2.contains("BODY"));
    }

    #[test]
    fn matches_app_by_substring() {
        let s = parse_skill("email", "---\ntriggers: mail, spark\n---\nbody");
        assert!(s.matches_app("Apple Mail", None));
        assert!(s.matches_app("X", Some("com.readdle.smartemail.spark")));
        assert!(!s.matches_app("Terminal", None));
    }

    #[test]
    fn all_starter_skills_parse_with_name_and_body() {
        for (stem, contents) in STARTER_SKILLS {
            let s = parse_skill(stem, contents);
            assert!(!s.name.is_empty(), "{stem} has empty name");
            assert!(!s.body.is_empty(), "{stem} has empty body");
            assert!(!s.description.is_empty(), "{stem} has empty description");
            // Body must not leak frontmatter.
            assert!(!s.body.starts_with("---"), "{stem} body leaked frontmatter");
        }
    }

    /// Build a small skill set for resolver tests.
    fn fixture_skills() -> Vec<Skill> {
        vec![
            parse_skill(
                "email",
                "---\nname: email\ntriggers: mail, spark\n---\nEMAIL BODY",
            ),
            parse_skill(
                "notes",
                "---\nname: notes\ntriggers: notion\n---\nNOTES BODY",
            ),
            parse_skill("empty", "---\nname: empty\n---\n   "), // body is whitespace only
        ]
    }

    #[test]
    fn resolve_sticky_beats_trigger() {
        let skills = fixture_skills();
        // Active = notes, but the focused app (Mail) matches email's trigger.
        let r = resolve_in(&skills, Some("notes"), "Apple Mail", None).unwrap();
        assert_eq!(r.name, "notes");
        assert!(r.prompt.contains("NOTES BODY"));
    }

    #[test]
    fn resolve_sticky_missing_falls_through_to_trigger() {
        let skills = fixture_skills();
        // Active points at a skill that isn't on disk; Mail triggers email.
        let r = resolve_in(&skills, Some("ghost"), "Apple Mail", None).unwrap();
        assert_eq!(r.name, "email");
    }

    #[test]
    fn resolve_no_sticky_uses_trigger() {
        let skills = fixture_skills();
        let r = resolve_in(&skills, None, "Notion", None).unwrap();
        assert_eq!(r.name, "notes");
    }

    #[test]
    fn resolve_no_match_returns_none() {
        let skills = fixture_skills();
        assert!(resolve_in(&skills, None, "Terminal", None).is_none());
    }

    #[test]
    fn cycle_next_rotates_auto_through_skills_and_back() {
        let skills = fixture_skills(); // order as built: email, notes, empty
        // Auto → first
        assert_eq!(cycle_next(&skills, None).as_deref(), Some("email"));
        // first → second
        assert_eq!(cycle_next(&skills, Some("email")).as_deref(), Some("notes"));
        // second → third
        assert_eq!(cycle_next(&skills, Some("notes")).as_deref(), Some("empty"));
        // last → Auto (None)
        assert_eq!(cycle_next(&skills, Some("empty")), None);
        // unknown current → first
        assert_eq!(cycle_next(&skills, Some("ghost")).as_deref(), Some("email"));
        // case-insensitive match: EMAIL is index 0 → next is notes
        assert_eq!(cycle_next(&skills, Some("EMAIL")).as_deref(), Some("notes"));
        // no skills → Auto
        assert_eq!(cycle_next(&[], Some("email")), None);
    }

    #[test]
    fn resolve_skips_empty_body_skill() {
        let skills = fixture_skills();
        // Explicitly active but bodyless → falls through; no trigger matches → None.
        assert!(resolve_in(&skills, Some("empty"), "Terminal", None).is_none());
    }

    #[test]
    fn list_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rekody_skill_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("email.md"),
            "---\nname: email\ndescription: d\n---\nBODY",
        )
        .unwrap();
        std::fs::write(
            dir.join("notes.md"),
            "---\nname: notes\ndescription: d\n---\nBODY",
        )
        .unwrap();
        // Non-md ignored.
        std::fs::write(dir.join("README.txt"), "ignore me").unwrap();

        let skills = list_skills_in(&dir);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "email"); // sorted
        assert_eq!(skills[1].name, "notes");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
