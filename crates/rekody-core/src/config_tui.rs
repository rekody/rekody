//! Interactive config TUI for `rekody config`.
//!
//! ratatui browser + editor for `~/.config/rekody/config.toml`. Sections on
//! the left, fields on the right. Pickers for enums, toggles for bools, text
//! input for free-form. Save writes the TOML back to disk via `toml::to_string`.
//!
//! Visual style intentionally mirrors `history_tui.rs` (the gold standard):
//! same brand palette, rounded card edges, soft spacing.
//!
//! Keybindings:
//!   ↑/k, ↓/j         navigate within active pane
//!   Tab, →, ←        switch focus between sections and fields
//!   Enter            edit selected field
//!   s                save changes to disk
//!   e                open config in $EDITOR (falls back if you'd rather)
//!   ?                help overlay
//!   q, Esc, Ctrl+C   quit (prompts if there are unsaved edits)

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};

use crate::RekodyConfig;

// ── Brand palette ───────────────────────────────────────────────────────────
// Duplicated from history_tui.rs intentionally — history_tui is the gold
// standard and must not be refactored.

const BRAND_TEAL: Color = Color::Rgb(0x20, 0x80, 0x8D);
const BRAND_TEAL_LIGHT: Color = Color::Rgb(0x4F, 0xB8, 0xC5);
const DIM: Color = Color::Rgb(0x77, 0x77, 0x77);
const SUBTLE: Color = Color::Rgb(0x55, 0x55, 0x55);
const FG: Color = Color::Rgb(0xE8, 0xE8, 0xE8);
const FG_BOLD: Color = Color::Rgb(0xFB, 0xFA, 0xF4);
const OK: Color = Color::Rgb(0x6B, 0xCB, 0x77);
const WARN: Color = Color::Rgb(0xE6, 0xB4, 0x50);
const ERR: Color = Color::Rgb(0xD9, 0x6B, 0x6B);

// ── Public entrypoint ───────────────────────────────────────────────────────

pub fn run(config: RekodyConfig, path: String) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config, path);
    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

// ── App state ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sections,
    Fields,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Stt,
    Llm,
    Activation,
    Audio,
    Injection,
}

impl SectionKind {
    const ALL: [Self; 5] = [
        Self::Stt,
        Self::Llm,
        Self::Activation,
        Self::Audio,
        Self::Injection,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Stt => "STT",
            Self::Llm => "LLM",
            Self::Activation => "Activation",
            Self::Audio => "Audio",
            Self::Injection => "Injection",
        }
    }
}

/// Which field within the active section is selected. Each section has its
/// own ordered field list; we identify a field by an enum-per-section.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldId {
    // STT
    SttEngine,
    WhisperModel,
    SttLanguage,
    CohereSttPort,
    // LLM
    LlmEnabled,
    // Activation
    ActivationMode,
    TriggerKey,
    MaxRecordingSecs,
    // Audio
    VadThreshold,
    RecordAllAudio,
    // Injection
    InjectionMethod,
}

impl FieldId {
    fn label(self) -> &'static str {
        match self {
            Self::SttEngine => "Engine",
            Self::WhisperModel => "Whisper model",
            Self::SttLanguage => "Language",
            Self::CohereSttPort => "Cohere port",
            Self::LlmEnabled => "Post-processing",
            Self::ActivationMode => "Mode",
            Self::TriggerKey => "Trigger key",
            Self::MaxRecordingSecs => "Max recording (s)",
            Self::VadThreshold => "VAD threshold",
            Self::RecordAllAudio => "Record all audio",
            Self::InjectionMethod => "Method",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::SttEngine => "Speech-to-text backend",
            Self::WhisperModel => "Local Whisper model size (only used if engine=local)",
            Self::SttLanguage => "BCP-47 code, e.g. en, sw, fr. Blank = auto-detect.",
            Self::CohereSttPort => {
                "Port for the local Cohere STT server (only used if engine=cohere)"
            }
            Self::LlmEnabled => "auto = on for Whisper engines, off for Deepgram",
            Self::ActivationMode => "How the hotkey starts/stops recording",
            Self::TriggerKey => "Which key triggers dictation",
            Self::MaxRecordingSecs => "Deadman switch in seconds (0 = unlimited)",
            Self::VadThreshold => "RMS gate (~0.01 typical). Lower = more sensitive.",
            Self::RecordAllAudio => "Bypass VAD — capture every frame",
            Self::InjectionMethod => "How transcribed text is inserted into the focused app",
        }
    }
}

fn fields_for(section: SectionKind, cfg: &RekodyConfig) -> Vec<FieldId> {
    match section {
        SectionKind::Stt => {
            let mut v = vec![FieldId::SttEngine];
            if cfg.stt_engine == "local" {
                v.push(FieldId::WhisperModel);
            }
            v.push(FieldId::SttLanguage);
            if cfg.stt_engine == "cohere" {
                v.push(FieldId::CohereSttPort);
            }
            v
        }
        SectionKind::Llm => vec![FieldId::LlmEnabled],
        SectionKind::Activation => vec![
            FieldId::ActivationMode,
            FieldId::TriggerKey,
            FieldId::MaxRecordingSecs,
        ],
        SectionKind::Audio => vec![FieldId::VadThreshold, FieldId::RecordAllAudio],
        SectionKind::Injection => vec![FieldId::InjectionMethod],
    }
}

/// Edit dialog state.
enum Editor {
    Picker {
        field: FieldId,
        options: Vec<&'static str>,
        index: usize,
    },
    Toggle {
        field: FieldId,
        options: Vec<&'static str>,
        index: usize,
    },
    Text {
        field: FieldId,
        buffer: String,
        error: Option<String>,
    },
}

struct Flash {
    msg: String,
    until: Instant,
    ok: bool,
}

struct App {
    config: RekodyConfig,
    path: String,
    dirty: bool,
    focus: Focus,
    section: usize,
    section_state: ListState,
    field_state: ListState,
    editor: Option<Editor>,
    confirm_quit: bool,
    show_help: bool,
    flash: Option<Flash>,
    quit: bool,
}

impl App {
    fn new(config: RekodyConfig, path: String) -> Self {
        let mut section_state = ListState::default();
        section_state.select(Some(0));
        let mut field_state = ListState::default();
        field_state.select(Some(0));
        Self {
            config,
            path,
            dirty: false,
            focus: Focus::Sections,
            section: 0,
            section_state,
            field_state,
            editor: None,
            confirm_quit: false,
            show_help: false,
            flash: None,
            quit: false,
        }
    }

    fn current_section(&self) -> SectionKind {
        SectionKind::ALL[self.section]
    }

    fn current_fields(&self) -> Vec<FieldId> {
        fields_for(self.current_section(), &self.config)
    }

    fn selected_field(&self) -> Option<FieldId> {
        let fields = self.current_fields();
        self.field_state
            .selected()
            .and_then(|i| fields.get(i).copied())
    }

    fn set_flash(&mut self, msg: impl Into<String>, ok: bool) {
        self.flash = Some(Flash {
            msg: msg.into(),
            until: Instant::now() + Duration::from_millis(2200),
            ok,
        });
    }

    fn save(&mut self) -> Result<()> {
        let serialized =
            toml::to_string_pretty(&self.config).map_err(|e| anyhow!("serialize config: {e}"))?;
        if let Some(parent) = std::path::Path::new(&self.path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&self.path, serialized).map_err(|e| anyhow!("write {}: {e}", self.path))?;
        self.dirty = false;
        Ok(())
    }
}

// ── Edit dialog construction ────────────────────────────────────────────────

/// Engines the STT picker offers. Nemotron, the flagship streaming engine
/// and the wizard's preselected default, leads when the build includes it.
#[cfg(feature = "nemotron")]
const STT_ENGINE_OPTIONS: &[&str] = &["nemotron", "local", "groq", "deepgram", "cohere"];
#[cfg(not(feature = "nemotron"))]
const STT_ENGINE_OPTIONS: &[&str] = &["local", "groq", "deepgram", "cohere"];

/// Whisper sizes Setup can actually download and the engine can load.
/// "base" is not one of them: the Pipeline maps it to turbo and the
/// downloader to tiny, so offering it here only misleads users.
const WHISPER_MODEL_OPTIONS: &[&str] = &["tiny", "small", "medium", "large", "turbo"];

fn open_editor_for(field: FieldId, cfg: &RekodyConfig) -> Editor {
    match field {
        FieldId::SttEngine => picker(field, STT_ENGINE_OPTIONS, &cfg.stt_engine),
        FieldId::WhisperModel => picker(field, WHISPER_MODEL_OPTIONS, &cfg.whisper_model),
        FieldId::ActivationMode => picker(
            field,
            &["push_to_talk", "toggle", "hold"],
            &cfg.activation_mode,
        ),
        FieldId::TriggerKey => picker(field, &["option_space", "fn_key"], &cfg.trigger_key),
        FieldId::InjectionMethod => picker(
            field,
            &["clipboard", "keystrokes", "applescript"],
            &cfg.injection_method,
        ),
        FieldId::LlmEnabled => {
            let current = match cfg.llm_enabled {
                None => "auto",
                Some(true) => "on",
                Some(false) => "off",
            };
            Editor::Toggle {
                field,
                options: vec!["auto", "on", "off"],
                index: ["auto", "on", "off"]
                    .iter()
                    .position(|o| *o == current)
                    .unwrap_or(0),
            }
        }
        FieldId::RecordAllAudio => {
            let current = if cfg.record_all_audio { "on" } else { "off" };
            Editor::Toggle {
                field,
                options: vec!["off", "on"],
                index: ["off", "on"]
                    .iter()
                    .position(|o| *o == current)
                    .unwrap_or(0),
            }
        }
        FieldId::SttLanguage => Editor::Text {
            field,
            buffer: cfg.stt_language.clone().unwrap_or_default(),
            error: None,
        },
        FieldId::CohereSttPort => Editor::Text {
            field,
            buffer: cfg.cohere_stt_port.to_string(),
            error: None,
        },
        FieldId::MaxRecordingSecs => Editor::Text {
            field,
            buffer: cfg.max_recording_secs.to_string(),
            error: None,
        },
        FieldId::VadThreshold => Editor::Text {
            field,
            buffer: format!("{}", cfg.vad_threshold),
            error: None,
        },
    }
}

fn picker(field: FieldId, options: &[&'static str], current: &str) -> Editor {
    let index = options.iter().position(|o| *o == current).unwrap_or(0);
    Editor::Picker {
        field,
        options: options.to_vec(),
        index,
    }
}

fn commit_editor(editor: &Editor, cfg: &mut RekodyConfig) -> Result<()> {
    match editor {
        Editor::Picker {
            field,
            options,
            index,
        } => {
            let val = options[*index];
            match field {
                FieldId::SttEngine => cfg.stt_engine = val.into(),
                FieldId::WhisperModel => cfg.whisper_model = val.into(),
                FieldId::ActivationMode => cfg.activation_mode = val.into(),
                FieldId::TriggerKey => cfg.trigger_key = val.into(),
                FieldId::InjectionMethod => cfg.injection_method = val.into(),
                _ => return Err(anyhow!("not a picker field")),
            }
        }
        Editor::Toggle {
            field,
            options,
            index,
        } => {
            let val = options[*index];
            match field {
                FieldId::LlmEnabled => {
                    cfg.llm_enabled = match val {
                        "auto" => None,
                        "on" => Some(true),
                        "off" => Some(false),
                        _ => return Err(anyhow!("invalid toggle option")),
                    }
                }
                FieldId::RecordAllAudio => cfg.record_all_audio = val == "on",
                _ => return Err(anyhow!("not a toggle field")),
            }
        }
        Editor::Text { field, buffer, .. } => match field {
            FieldId::SttLanguage => {
                cfg.stt_language = if buffer.trim().is_empty() {
                    None
                } else {
                    Some(buffer.trim().into())
                };
            }
            FieldId::CohereSttPort => {
                cfg.cohere_stt_port = buffer
                    .trim()
                    .parse()
                    .map_err(|_| anyhow!("port must be an integer 0–65535"))?;
            }
            FieldId::MaxRecordingSecs => {
                cfg.max_recording_secs = buffer
                    .trim()
                    .parse()
                    .map_err(|_| anyhow!("must be a non-negative integer"))?;
            }
            FieldId::VadThreshold => {
                cfg.vad_threshold = buffer
                    .trim()
                    .parse()
                    .map_err(|_| anyhow!("must be a decimal number, e.g. 0.01"))?;
            }
            _ => return Err(anyhow!("not a text field")),
        },
    }
    Ok(())
}

// ── Run loop ────────────────────────────────────────────────────────────────

fn run_loop<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let tick = Duration::from_millis(100);
    let mut last = Instant::now();

    loop {
        terminal.draw(|f| render(f, app))?;

        let timeout = tick
            .checked_sub(last.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(app, key)?;
        }
        if last.elapsed() >= tick {
            // Expire flash.
            if let Some(f) = &app.flash
                && Instant::now() > f.until
            {
                app.flash = None;
            }
            last = Instant::now();
        }
        if app.quit {
            return Ok(());
        }
    }
}

// ── Input handling ──────────────────────────────────────────────────────────

fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // Confirm-quit modal takes precedence.
    if app.confirm_quit {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => app.quit = true,
            _ => app.confirm_quit = false,
        }
        return Ok(());
    }
    // Help overlay.
    if app.show_help {
        app.show_help = false;
        return Ok(());
    }
    // Editor modal.
    if app.editor.is_some() {
        return handle_editor_key(app, key);
    }
    // Global keys.
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('q'), _)
        | (KeyCode::Esc, _) => {
            if app.dirty {
                app.confirm_quit = true;
            } else {
                app.quit = true;
            }
        }
        (KeyCode::Char('?'), _) => app.show_help = true,
        (KeyCode::Char('s'), _) => match app.save() {
            Ok(()) => app.set_flash(format!("Saved to {}", app.path), true),
            Err(e) => app.set_flash(format!("Save failed: {e}"), false),
        },
        (KeyCode::Char('e'), _) => open_external_editor(app)?,
        (KeyCode::Tab, _) | (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
            app.focus = Focus::Fields;
        }
        (KeyCode::BackTab, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
            app.focus = Focus::Sections;
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => move_selection(app, -1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => move_selection(app, 1),
        (KeyCode::Enter, _) => {
            if app.focus == Focus::Sections {
                app.focus = Focus::Fields;
            } else if let Some(field) = app.selected_field() {
                app.editor = Some(open_editor_for(field, &app.config));
            }
        }
        _ => {}
    }
    Ok(())
}

fn move_selection(app: &mut App, delta: i32) {
    match app.focus {
        Focus::Sections => {
            let n = SectionKind::ALL.len() as i32;
            let cur = app.section as i32;
            let next = (cur + delta).rem_euclid(n) as usize;
            app.section = next;
            app.section_state.select(Some(next));
            app.field_state.select(Some(0));
        }
        Focus::Fields => {
            let fields = app.current_fields();
            if fields.is_empty() {
                return;
            }
            let n = fields.len() as i32;
            let cur = app.field_state.selected().unwrap_or(0) as i32;
            let next = (cur + delta).rem_euclid(n) as usize;
            app.field_state.select(Some(next));
        }
    }
}

fn handle_editor_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let mut close = false;
    let mut commit = false;
    if let Some(editor) = app.editor.as_mut() {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => close = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => close = true,
            (KeyCode::Enter, _) => commit = true,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => match editor {
                Editor::Picker { index, options, .. } | Editor::Toggle { index, options, .. } => {
                    let n = options.len() as i32;
                    *index = ((*index as i32 - 1).rem_euclid(n)) as usize;
                }
                Editor::Text { .. } => {}
            },
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => match editor {
                Editor::Picker { index, options, .. } | Editor::Toggle { index, options, .. } => {
                    let n = options.len() as i32;
                    *index = ((*index as i32 + 1).rem_euclid(n)) as usize;
                }
                Editor::Text { .. } => {}
            },
            (KeyCode::Left, _) | (KeyCode::Right, _) => {
                if let Editor::Toggle { index, options, .. } = editor {
                    let n = options.len() as i32;
                    let d = if matches!(key.code, KeyCode::Right) {
                        1
                    } else {
                        -1
                    };
                    *index = ((*index as i32 + d).rem_euclid(n)) as usize;
                }
            }
            (KeyCode::Backspace, _) => {
                if let Editor::Text { buffer, error, .. } = editor {
                    buffer.pop();
                    *error = None;
                }
            }
            (KeyCode::Char(c), _) => {
                if let Editor::Text { buffer, error, .. } = editor {
                    buffer.push(c);
                    *error = None;
                }
            }
            _ => {}
        }
    }
    if commit {
        // Take ownership of the editor so we can move buffers into the config.
        let editor = app.editor.take().unwrap();
        match commit_editor(&editor, &mut app.config) {
            Ok(()) => {
                app.dirty = true;
                let label = match &editor {
                    Editor::Picker { field, .. }
                    | Editor::Toggle { field, .. }
                    | Editor::Text { field, .. } => field.label(),
                };
                app.set_flash(format!("{label} updated (unsaved — press s)"), true);
            }
            Err(e) => {
                // Put it back, attach error to text editors so the user sees it inline.
                let with_err = match editor {
                    Editor::Text { field, buffer, .. } => Editor::Text {
                        field,
                        buffer,
                        error: Some(e.to_string()),
                    },
                    other => other,
                };
                app.editor = Some(with_err);
            }
        }
    } else if close {
        app.editor = None;
    }
    Ok(())
}

fn open_external_editor(app: &mut App) -> Result<()> {
    // Tear down the alt-screen so $EDITOR can take over the terminal cleanly.
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "nano".to_string());
    let status = std::process::Command::new(&editor).arg(&app.path).status();

    // Restore alt-screen and reload from disk so external edits show up.
    enable_raw_mode().ok();
    execute!(io::stdout(), EnterAlternateScreen).ok();

    match status {
        Ok(s) if s.success() => {
            if let Ok(contents) = std::fs::read_to_string(&app.path)
                && let Ok(cfg) = toml::from_str::<RekodyConfig>(&contents)
            {
                app.config = cfg;
                app.dirty = false;
                app.set_flash("Reloaded from disk", true);
            }
        }
        _ => app.set_flash("Editor exited non-zero", false),
    }
    Ok(())
}

// ── Rendering ───────────────────────────────────────────────────────────────

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_header(f, chunks[0], app);
    render_body(f, chunks[1], app);
    render_footer(f, chunks[2], app);

    if let Some(editor) = &app.editor {
        render_editor_modal(f, area, editor);
    }
    if app.show_help {
        render_help(f, area);
    }
    if app.confirm_quit {
        render_confirm_quit(f, area);
    }
}

fn render_header(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let dirty_tag = if app.dirty {
        Span::styled("  ● unsaved", Style::default().fg(WARN))
    } else {
        Span::raw("")
    };
    let line = Line::from(vec![
        Span::styled(
            "  rekody config",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", app.path), Style::default().fg(DIM)),
        dirty_tag,
    ]);
    let p = Paragraph::new(line);
    f.render_widget(p, area);
}

fn render_body(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(0)])
        .split(area);

    render_sections_pane(f, chunks[0], app);
    render_fields_pane(f, chunks[1], app);
}

fn render_sections_pane(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = SectionKind::ALL
        .iter()
        .map(|s| {
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(s.label(), Style::default().fg(FG)),
            ]))
        })
        .collect();
    let border_color = if app.focus == Focus::Sections {
        BRAND_TEAL_LIGHT
    } else {
        SUBTLE
    };
    let highlight = if app.focus == Focus::Sections {
        Style::default()
            .fg(FG_BOLD)
            .bg(BRAND_TEAL)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(BRAND_TEAL_LIGHT)
            .add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    " Sections ",
                    Style::default().fg(BRAND_TEAL_LIGHT),
                ))
                .padding(Padding::new(0, 0, 1, 0)),
        )
        .highlight_style(highlight)
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.section_state);
}

fn render_fields_pane(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let fields = app.current_fields();
    let cfg = &app.config;
    let items: Vec<ListItem> = fields
        .iter()
        .map(|fid| {
            let value = current_value_str(*fid, cfg);
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<22}", fid.label()), Style::default().fg(DIM)),
                Span::styled(
                    value,
                    Style::default().fg(FG_BOLD).add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();

    let border_color = if app.focus == Focus::Fields {
        BRAND_TEAL_LIGHT
    } else {
        SUBTLE
    };
    let highlight = if app.focus == Focus::Fields {
        Style::default()
            .fg(FG_BOLD)
            .bg(BRAND_TEAL)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(BRAND_TEAL_LIGHT)
    };

    let title = format!(" {} ", app.current_section().label());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(title, Style::default().fg(BRAND_TEAL_LIGHT)))
                .padding(Padding::new(0, 0, 1, 0)),
        )
        .highlight_style(highlight)
        .highlight_symbol("▶ ");

    // Split off the bottom of the pane for the field hint.
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    f.render_stateful_widget(list, inner[0], &mut app.field_state);

    let hint = app.selected_field().map(|fid| fid.hint()).unwrap_or("");
    let extra = providers_hint(&app.config);
    let hint_line = if extra.is_empty() {
        Line::from(Span::styled(format!("  {hint}"), Style::default().fg(DIM)))
    } else {
        Line::from(vec![
            Span::styled(format!("  {hint}"), Style::default().fg(DIM)),
            Span::raw("    "),
            Span::styled(extra, Style::default().fg(SUBTLE)),
        ])
    };
    let p = Paragraph::new(hint_line).wrap(Wrap { trim: true });
    f.render_widget(p, inner[1]);
}

fn current_value_str(field: FieldId, cfg: &RekodyConfig) -> String {
    match field {
        FieldId::SttEngine => cfg.stt_engine.clone(),
        FieldId::WhisperModel => cfg.whisper_model.clone(),
        FieldId::SttLanguage => cfg.stt_language.clone().unwrap_or_else(|| "auto".into()),
        FieldId::CohereSttPort => cfg.cohere_stt_port.to_string(),
        FieldId::LlmEnabled => match cfg.llm_enabled {
            None => "auto".into(),
            Some(true) => "on".into(),
            Some(false) => "off".into(),
        },
        FieldId::ActivationMode => cfg.activation_mode.clone(),
        FieldId::TriggerKey => cfg.trigger_key.clone(),
        FieldId::MaxRecordingSecs => {
            if cfg.max_recording_secs == 0 {
                "unlimited".into()
            } else {
                cfg.max_recording_secs.to_string()
            }
        }
        FieldId::VadThreshold => format!("{}", cfg.vad_threshold),
        FieldId::RecordAllAudio => if cfg.record_all_audio { "on" } else { "off" }.into(),
        FieldId::InjectionMethod => cfg.injection_method.clone(),
    }
}

fn providers_hint(cfg: &RekodyConfig) -> String {
    if cfg.providers.is_empty() {
        return String::new();
    }
    let names: Vec<String> = cfg.providers.iter().map(|p| p.name.clone()).collect();
    format!("LLM providers (edit with `e`): {}", names.join(", "))
}

fn render_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let mut spans = vec![
        key_hint("↑↓"),
        Span::raw(" nav  "),
        key_hint("Tab"),
        Span::raw(" pane  "),
        key_hint("⏎"),
        Span::raw(" edit  "),
        key_hint("s"),
        Span::raw(" save  "),
        key_hint("e"),
        Span::raw(" $EDITOR  "),
        key_hint("?"),
        Span::raw(" help  "),
        key_hint("q"),
        Span::raw(" quit"),
    ];
    if let Some(flash) = &app.flash {
        spans.push(Span::raw("    "));
        let color = if flash.ok { OK } else { ERR };
        spans.push(Span::styled(
            flash.msg.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line).style(Style::default().fg(DIM)), area);
}

fn key_hint(label: &str) -> Span<'_> {
    Span::styled(
        format!(" {label} "),
        Style::default().fg(FG_BOLD).bg(SUBTLE),
    )
}

// ── Modals ──────────────────────────────────────────────────────────────────

fn render_editor_modal(f: &mut ratatui::Frame, area: Rect, editor: &Editor) {
    let modal = centered_rect(60, 50, area);
    f.render_widget(Clear, modal);

    let (title, body): (String, Vec<Line>) = match editor {
        Editor::Picker {
            field,
            options,
            index,
        }
        | Editor::Toggle {
            field,
            options,
            index,
        } => {
            let title = format!(" Edit  {} ", field.label());
            let mut lines = vec![
                Line::from(Span::styled(
                    format!("  {}", field.hint()),
                    Style::default().fg(DIM),
                )),
                Line::from(""),
            ];
            for (i, opt) in options.iter().enumerate() {
                let sel = i == *index;
                let style = if sel {
                    Style::default()
                        .fg(FG_BOLD)
                        .bg(BRAND_TEAL)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(FG)
                };
                let marker = if sel { "  ▶ " } else { "    " };
                lines.push(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(*opt, style),
                ]));
            }
            (title, lines)
        }
        Editor::Text {
            field,
            buffer,
            error,
        } => {
            let title = format!(" Edit  {} ", field.label());
            let mut lines = vec![
                Line::from(Span::styled(
                    format!("  {}", field.hint()),
                    Style::default().fg(DIM),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{}_", buffer),
                        Style::default().fg(FG_BOLD).add_modifier(Modifier::BOLD),
                    ),
                ]),
            ];
            if let Some(err) = error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  {err}"),
                    Style::default().fg(ERR),
                )));
            }
            (title, lines)
        }
    };

    let footer_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  ⏎ confirm   Esc cancel",
            Style::default().fg(DIM),
        )),
    ];
    let mut all = body;
    all.extend(footer_lines);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BRAND_TEAL_LIGHT))
        .title(Span::styled(
            title,
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::new(1, 1, 1, 1));
    let p = Paragraph::new(all).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, modal);
}

fn render_help(f: &mut ratatui::Frame, area: Rect) {
    let modal = centered_rect(60, 60, area);
    f.render_widget(Clear, modal);
    let lines = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  ↑/k, ↓/j        navigate"),
        Line::from("  Tab / → / ←     switch pane"),
        Line::from("  Enter           edit selected field"),
        Line::from("  s               save to disk"),
        Line::from("  e               open in $EDITOR (auto-reloads)"),
        Line::from("  ?               toggle this help"),
        Line::from("  q / Esc         quit (prompts if unsaved)"),
        Line::from(""),
        Line::from(Span::styled(
            "Notes",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  - API keys are managed with `rekody key set/delete`",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "  - LLM provider list edits via `e` for now",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "  - Save serializes the full config; existing TOML comments are not preserved",
            Style::default().fg(DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Press any key to dismiss",
            Style::default().fg(SUBTLE),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BRAND_TEAL_LIGHT))
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::new(2, 2, 1, 1));
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn render_confirm_quit(f: &mut ratatui::Frame, area: Rect) {
    let modal = centered_rect(46, 22, area);
    f.render_widget(Clear, modal);
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  You have unsaved changes.",
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Quit anyway?  [y/N]",
            Style::default().fg(FG),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  (or press s before quitting to save)",
            Style::default().fg(DIM),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(WARN))
        .title(Span::styled(
            " Discard changes? ",
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::new(2, 2, 0, 0))
        .title_alignment(Alignment::Center);
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_y = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_y[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker_options(field: FieldId) -> Vec<&'static str> {
        match open_editor_for(field, &RekodyConfig::default()) {
            Editor::Picker { options, .. } => options,
            _ => panic!("expected a picker editor"),
        }
    }

    /// The engine picker must match what this build's pipeline supports.
    /// The regression: nemotron, the flagship engine, was not selectable.
    #[test]
    fn stt_engine_picker_matches_supported_engines() {
        let opts = picker_options(FieldId::SttEngine);
        #[cfg(feature = "nemotron")]
        assert_eq!(
            opts,
            vec!["nemotron", "local", "groq", "deepgram", "cohere"]
        );
        #[cfg(not(feature = "nemotron"))]
        assert_eq!(opts, vec!["local", "groq", "deepgram", "cohere"]);
    }

    /// Every size offered maps to a model Setup downloads and the engine
    /// loads. "base" was neither (Pipeline mapped it to turbo, the
    /// downloader to tiny) and must stay out of the list.
    #[test]
    fn whisper_model_picker_offers_only_supported_sizes() {
        let opts = picker_options(FieldId::WhisperModel);
        assert_eq!(opts, vec!["tiny", "small", "medium", "large", "turbo"]);
        assert!(!opts.contains(&"base"));
    }
}
