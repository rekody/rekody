//! Context-specific LLM prompt management for voice dictation cleanup.
//!
//! Provides system prompts that instruct the LLM how to clean up raw
//! speech-to-text output based on the application the user is dictating into.

/// Base system prompt shared by all contexts.
const BASE_PROMPT: &str = "\
You are a strict voice dictation cleanup tool. You receive raw speech-to-text output and clean it up.

CRITICAL RULES:
- Output ONLY what the user actually said. NEVER add new content, expand on ideas, elaborate, or generate text the user did not speak.
- Remove filler words: \"um\", \"uh\", \"like\" (when used as filler), \"you know\", \"I mean\", \"sort of\", \"kind of\" (when used as filler).
- Fix grammar, spelling, and punctuation.
- Add appropriate capitalization.
- Resolve self-corrections: when the speaker restarts or changes direction mid-sentence, keep only the final intended version.
- If the user said one sentence, output one sentence. If they said a phrase, output a phrase. Match the length and scope of what was spoken.
- NEVER add explanations, context, examples, or elaboration that the user did not say.
- NEVER wrap output in quotes or markdown.
- Output the cleaned text and NOTHING else.";

/// Additional instructions for code editor contexts.
const CODE_EDITOR_CONTEXT: &str =
    "\n\nAdditional context: the user is dictating into a CODE EDITOR.
- Preserve all technical terms, function names, variable names, and identifiers exactly.
- Convert spoken operators to their symbolic form:
  - \"equals equals\" or \"double equals\" → \"==\"
  - \"not equals\" or \"bang equals\" → \"!=\"
  - \"triple equals\" or \"strict equals\" → \"===\"
  - \"greater than or equal\" → \">=\"
  - \"less than or equal\" → \"<=\"
  - \"open paren\" or \"left paren\" → \"(\"
  - \"close paren\" or \"right paren\" → \")\"
  - \"open bracket\" or \"left bracket\" → \"[\"
  - \"close bracket\" or \"right bracket\" → \"]\"
  - \"open brace\" or \"left brace\" or \"open curly\" → \"{\"
  - \"close brace\" or \"right brace\" or \"close curly\" → \"}\"
  - \"arrow\" or \"fat arrow\" → \"=>\"
  - \"dash greater than\" or \"thin arrow\" → \"->\"
  - \"plus equals\" → \"+=\"
  - \"minus equals\" → \"-=\"
  - \"ampersand ampersand\" or \"and and\" → \"&&\"
  - \"pipe pipe\" or \"or or\" → \"||\"
  - \"colon colon\" or \"double colon\" → \"::\"
  - \"semicolon\" → \";\"
- Convert spoken keywords: \"new line\" → actual newline, \"tab\" → actual tab character.
- If the user is clearly dictating a comment, format it as a code comment.";

/// Additional instructions for messaging app contexts.
const MESSAGING_CONTEXT: &str =
    "\n\nAdditional context: the user is dictating into a MESSAGING APP.
- Keep the tone casual and conversational.
- Use short sentences.
- Contractions are preferred (\"don't\" over \"do not\").
- Emojis mentioned by name can be kept as-is (the user will handle emoji conversion).
- Do not add formal greetings or sign-offs unless the user explicitly dictated them.";

/// Additional instructions for email client contexts.
const EMAIL_CONTEXT: &str = "\n\nAdditional context: the user is dictating into an EMAIL CLIENT.
- Use professional prose with complete sentences.
- Maintain proper paragraph structure.
- Preserve greetings (\"Hi [name]\", \"Dear [name]\") and closings (\"Best regards\", \"Thanks\") if spoken.
- Slightly more formal tone than messaging, but still natural.
- Organize longer dictations into logical paragraphs.";

/// Additional instructions for document editor contexts.
const DOCUMENT_CONTEXT: &str =
    "\n\nAdditional context: the user is dictating into a DOCUMENT EDITOR.
- Format text in full, well-structured paragraphs.
- Use proper sentence structure and transitions.
- \"New paragraph\" should start a new paragraph (double newline).
- Maintain consistent tone throughout.
- Preserve any headings or structural cues the user dictates.";

/// Additional instructions for terminal contexts.
const TERMINAL_CONTEXT: &str = "\n\nAdditional context: the user is dictating into a TERMINAL / command line.
- Preserve commands, file paths, flags, and arguments exactly as spoken.
- Convert spoken path separators: \"slash\" → \"/\", \"backslash\" → \"\\\".
- Convert spoken special characters: \"tilde\" → \"~\", \"dot\" → \".\", \"dash\" → \"-\", \"double dash\" → \"--\".
- \"pipe\" → \"|\", \"redirect\" or \"greater than\" → \">\", \"append\" or \"double greater than\" → \">>\".
- Do not add punctuation to commands. Commands should be output as-is.
- If the user is clearly dictating a command, output it on a single line with no trailing period.";

/// Additional instructions for the default / general-purpose context.
const DEFAULT_CONTEXT: &str = "\n\nAdditional context: general-purpose dictation.
- Use clear, natural prose.
- Apply standard English punctuation and capitalization rules.
- Format for readability.";

/// Application context categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppContext {
    /// Code editors: VS Code, Cursor, Vim, Neovim, IntelliJ, etc.
    CodeEditor,
    /// Messaging apps: Slack, Discord, iMessage, WhatsApp, Telegram, etc.
    Messaging,
    /// Email clients: Mail, Outlook, Gmail (web), Thunderbird, etc.
    Email,
    /// Document editors: Google Docs, Word, Pages, Notion, etc.
    Document,
    /// Terminal emulators: Terminal, iTerm, Alacritty, Warp, kitty, etc.
    Terminal,
    /// Fallback for unrecognized applications.
    Default,
}

impl AppContext {
    /// Returns the context-specific portion of the system prompt.
    fn context_prompt(self) -> &'static str {
        match self {
            Self::CodeEditor => CODE_EDITOR_CONTEXT,
            Self::Messaging => MESSAGING_CONTEXT,
            Self::Email => EMAIL_CONTEXT,
            Self::Document => DOCUMENT_CONTEXT,
            Self::Terminal => TERMINAL_CONTEXT,
            Self::Default => DEFAULT_CONTEXT,
        }
    }

    /// Returns the full system prompt (base + context-specific).
    pub fn system_prompt(self) -> String {
        format!("{}{}", BASE_PROMPT, self.context_prompt())
    }
}

/// Detect the application context from an app name or bundle ID.
///
/// Matching is case-insensitive. The function checks the app name first,
/// then falls back to the bundle ID if provided.
pub fn detect_context(app_name: &str, bundle_id: Option<&str>) -> AppContext {
    let name = app_name.to_lowercase();

    // Check app name against known patterns.
    if let Some(ctx) = match_name(&name) {
        return ctx;
    }

    // Check bundle ID if provided.
    if let Some(bid) = bundle_id {
        let bid_lower = bid.to_lowercase();
        if let Some(ctx) = match_bundle_id(&bid_lower) {
            return ctx;
        }
    }

    AppContext::Default
}

/// Returns the full system prompt for a given application.
///
/// This is the primary entry point. It detects the app context from the
/// app name and optional bundle ID, then returns the combined system prompt.
///
/// # Examples
///
/// ```
/// use rekody_core::prompts::get_prompt_for_app;
///
/// let prompt = get_prompt_for_app("Visual Studio Code", None);
/// assert!(prompt.contains("CODE EDITOR"));
///
/// let prompt = get_prompt_for_app("Slack", None);
/// assert!(prompt.contains("MESSAGING APP"));
///
/// let prompt = get_prompt_for_app("SomeRandomApp", None);
/// assert!(prompt.contains("general-purpose"));
/// ```
pub fn get_prompt_for_app(app_name: &str, bundle_id: Option<&str>) -> String {
    detect_context(app_name, bundle_id).system_prompt()
}

/// Match an app name (already lowercased) to a context.
fn match_name(name: &str) -> Option<AppContext> {
    // Code editors
    if name.contains("code")
        || name.contains("cursor")
        || name.contains("vim")
        || name.contains("neovim")
        || name.contains("nvim")
        || name.contains("intellij")
        || name.contains("webstorm")
        || name.contains("pycharm")
        || name.contains("rustrover")
        || name.contains("clion")
        || name.contains("goland")
        || name.contains("rider")
        || name.contains("android studio")
        || name.contains("xcode")
        || name.contains("sublime")
        || name.contains("atom")
        || name.contains("emacs")
        || name.contains("zed")
        || name.contains("nova")
        || name.contains("bbedit")
        || name.contains("textmate")
        || name.contains("windsurf")
    {
        return Some(AppContext::CodeEditor);
    }

    // Messaging apps
    if name.contains("slack")
        || name.contains("discord")
        || name.contains("messages")
        || name.contains("imessage")
        || name.contains("whatsapp")
        || name.contains("telegram")
        || name.contains("signal")
        || name.contains("teams")
        || name.contains("zoom chat")
        || name.contains("messenger")
        || name.contains("element")
    {
        return Some(AppContext::Messaging);
    }

    // Email clients
    if name == "mail"
        || name.contains("outlook")
        || name.contains("gmail")
        || name.contains("thunderbird")
        || name.contains("spark")
        || name.contains("airmail")
        || name.contains("mimestream")
        || name.contains("fastmail")
        || name.contains("protonmail")
        || name.contains("proton mail")
        || name.contains("superhuman")
    {
        return Some(AppContext::Email);
    }

    // Document editors
    if name.contains("docs")
        || name.contains("word")
        || name.contains("pages")
        || name.contains("notion")
        || name.contains("obsidian")
        || name.contains("bear")
        || name.contains("ulysses")
        || name.contains("scrivener")
        || name.contains("ia writer")
        || name.contains("google docs")
        || name.contains("libreoffice writer")
        || name.contains("textedit")
    {
        return Some(AppContext::Document);
    }

    // Terminal emulators
    if name.contains("terminal")
        || name.contains("iterm")
        || name.contains("alacritty")
        || name.contains("kitty")
        || name.contains("warp")
        || name.contains("hyper")
        || name.contains("wezterm")
        || name.contains("ghostty")
        || name.contains("rio")
    {
        return Some(AppContext::Terminal);
    }

    None
}

/// Match a bundle ID (already lowercased) to a context.
fn match_bundle_id(bid: &str) -> Option<AppContext> {
    // Code editors
    if bid.contains("com.microsoft.vscode")
        || bid.contains("com.todesktop.cursor")
        || bid.contains("org.vim.")
        || bid.contains("com.jetbrains.")
        || bid.contains("com.apple.dt.xcode")
        || bid.contains("com.sublimetext.")
        || bid.contains("com.github.atom")
        || bid.contains("org.gnu.emacs")
        || bid.contains("dev.zed.")
        || bid.contains("com.panic.nova")
        || bid.contains("com.barebones.bbedit")
        || bid.contains("com.macromates.textmate")
        || bid.contains("com.codeium.windsurf")
    {
        return Some(AppContext::CodeEditor);
    }

    // Messaging apps
    if bid.contains("com.tinyspeck.slackmacgap")
        || bid.contains("com.hnc.discord")
        || bid.contains("com.apple.mobilesms")
        || bid.contains("com.apple.messages")
        || bid.contains("net.whatsapp.")
        || bid.contains("org.telegram.")
        || bid.contains("org.whispersystems.signal")
        || bid.contains("com.microsoft.teams")
    {
        return Some(AppContext::Messaging);
    }

    // Email clients
    if bid.contains("com.apple.mail")
        || bid.contains("com.microsoft.outlook")
        || bid.contains("org.mozilla.thunderbird")
        || bid.contains("com.readdle.spark")
        || bid.contains("it.bloop.airmail")
        || bid.contains("com.mimestream.mimestream")
        || bid.contains("com.superhuman.")
    {
        return Some(AppContext::Email);
    }

    // Document editors
    if bid.contains("com.apple.iwork.pages")
        || bid.contains("com.microsoft.word")
        || bid.contains("notion.id")
        || bid.contains("md.obsidian")
        || bid.contains("net.shinyfrog.bear")
        || bid.contains("com.ulyssesapp.")
        || bid.contains("com.apple.textedit")
    {
        return Some(AppContext::Document);
    }

    // Terminal emulators
    if bid.contains("com.apple.terminal")
        || bid.contains("com.googlecode.iterm2")
        || bid.contains("org.alacritty")
        || bid.contains("net.kovidgoyal.kitty")
        || bid.contains("dev.warp.warp")
        || bid.contains("co.zeit.hyper")
        || bid.contains("com.github.wez.wezterm")
        || bid.contains("com.mitchellh.ghostty")
    {
        return Some(AppContext::Terminal);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_editor_detection() {
        assert_eq!(
            detect_context("Visual Studio Code", None),
            AppContext::CodeEditor
        );
        assert_eq!(detect_context("Cursor", None), AppContext::CodeEditor);
        assert_eq!(detect_context("Neovim", None), AppContext::CodeEditor);
        assert_eq!(detect_context("Xcode", None), AppContext::CodeEditor);
    }

    #[test]
    fn test_messaging_detection() {
        assert_eq!(detect_context("Slack", None), AppContext::Messaging);
        assert_eq!(detect_context("Discord", None), AppContext::Messaging);
        assert_eq!(detect_context("WhatsApp", None), AppContext::Messaging);
    }

    #[test]
    fn test_email_detection() {
        assert_eq!(detect_context("Mail", None), AppContext::Email);
        assert_eq!(detect_context("Outlook", None), AppContext::Email);
        assert_eq!(detect_context("Superhuman", None), AppContext::Email);
    }

    #[test]
    fn test_document_detection() {
        assert_eq!(detect_context("Google Docs", None), AppContext::Document);
        assert_eq!(detect_context("Pages", None), AppContext::Document);
        assert_eq!(detect_context("Notion", None), AppContext::Document);
    }

    #[test]
    fn test_terminal_detection() {
        assert_eq!(detect_context("Terminal", None), AppContext::Terminal);
        assert_eq!(detect_context("iTerm2", None), AppContext::Terminal);
        assert_eq!(detect_context("Ghostty", None), AppContext::Terminal);
        assert_eq!(detect_context("Warp", None), AppContext::Terminal);
    }

    #[test]
    fn test_default_fallback() {
        assert_eq!(detect_context("SomeRandomApp", None), AppContext::Default);
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(detect_context("SLACK", None), AppContext::Messaging);
        assert_eq!(
            detect_context("visual studio code", None),
            AppContext::CodeEditor
        );
        assert_eq!(detect_context("TERMINAL", None), AppContext::Terminal);
    }

    #[test]
    fn test_bundle_id_fallback() {
        assert_eq!(
            detect_context("Unknown", Some("com.apple.mail")),
            AppContext::Email
        );
        assert_eq!(
            detect_context("Unknown", Some("com.tinyspeck.slackmacgap")),
            AppContext::Messaging
        );
        assert_eq!(
            detect_context("Unknown", Some("com.mitchellh.ghostty")),
            AppContext::Terminal
        );
    }

    #[test]
    fn test_app_name_takes_precedence_over_bundle_id() {
        // App name matches code editor, bundle ID matches terminal.
        // App name should win.
        assert_eq!(
            detect_context("Cursor", Some("com.apple.terminal")),
            AppContext::CodeEditor
        );
    }

    #[test]
    fn test_get_prompt_for_app_contains_base() {
        let prompt = get_prompt_for_app("Slack", None);
        assert!(prompt.contains("voice dictation cleanup tool"));
        assert!(prompt.contains("filler words"));
    }

    #[test]
    fn test_get_prompt_for_app_contains_context() {
        let prompt = get_prompt_for_app("Visual Studio Code", None);
        assert!(prompt.contains("CODE EDITOR"));
        assert!(prompt.contains("equals equals"));

        let prompt = get_prompt_for_app("Slack", None);
        assert!(prompt.contains("MESSAGING APP"));

        let prompt = get_prompt_for_app("Mail", None);
        assert!(prompt.contains("EMAIL CLIENT"));

        let prompt = get_prompt_for_app("Notion", None);
        assert!(prompt.contains("DOCUMENT EDITOR"));

        let prompt = get_prompt_for_app("Terminal", None);
        assert!(prompt.contains("TERMINAL"));

        let prompt = get_prompt_for_app("RandomApp", None);
        assert!(prompt.contains("general-purpose"));
    }
}
