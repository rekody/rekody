//! Interactive skill picker for `rekody skill`.
//!
//! A sticky list-picker: choose the skill that reshapes your dictation (email,
//! notes, spec, …) and it stays active across dictations until you change it.
//! Selecting "Auto" clears the active skill and returns to built-in app
//! detection. Same visual language as `history_tui.rs` (gold standard),
//! `config_tui.rs`, and `key_tui.rs`.
//!
//! Keybindings:
//!   ↑/k, ↓/j    navigate
//!   Enter       activate the selected skill (or Auto to clear)
//!   n           clear the active skill (back to Auto)
//!   r           reload skills from disk
//!   ?           help
//!   q, Esc      quit

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};

use crate::skill;

// ── Brand palette (duplicated from history_tui.rs intentionally) ────────────

const BRAND_TEAL: Color = Color::Rgb(0x20, 0x80, 0x8D);
const BRAND_TEAL_LIGHT: Color = Color::Rgb(0x4F, 0xB8, 0xC5);
const DIM: Color = Color::Rgb(0x77, 0x77, 0x77);
const SUBTLE: Color = Color::Rgb(0x55, 0x55, 0x55);
const FG: Color = Color::Rgb(0xE8, 0xE8, 0xE8);
const FG_BOLD: Color = Color::Rgb(0xFB, 0xFA, 0xF4);
const OK: Color = Color::Rgb(0x6B, 0xCB, 0x77);
const WARN: Color = Color::Rgb(0xE6, 0xB4, 0x50);

// ── Public entrypoint ───────────────────────────────────────────────────────

/// `llm_active` reflects whether LLM post-processing is on; when false, skills
/// are inert and the picker says so.
pub fn run(llm_active: bool) -> Result<()> {
    // Seed the starter pack on first use so the picker is never empty.
    skill::ensure_starter_pack().ok();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(llm_active);
    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

// ── App state ───────────────────────────────────────────────────────────────

/// A row in the picker. The first row is the synthetic "Auto" entry that
/// clears the active skill; the rest are real skills.
struct Row {
    /// Skill name, or empty for the Auto row.
    name: String,
    description: String,
    is_auto: bool,
}

struct Flash {
    msg: String,
    until: Instant,
    ok: bool,
}

struct App {
    rows: Vec<Row>,
    /// Currently active skill name (None = Auto).
    active: Option<String>,
    /// Whether LLM post-processing is on; if not, skills are inert.
    llm_active: bool,
    list_state: ListState,
    show_help: bool,
    flash: Option<Flash>,
    quit: bool,
}

impl App {
    fn new(llm_active: bool) -> Self {
        let mut app = Self {
            rows: Vec::new(),
            active: None,
            llm_active,
            list_state: ListState::default(),
            show_help: false,
            flash: None,
            quit: false,
        };
        app.reload();
        // Place the cursor on the active row (or Auto) initially.
        let idx = app.active_row_index();
        app.list_state.select(Some(idx));
        app
    }

    fn reload(&mut self) {
        self.active = skill::active_name();
        let mut rows = vec![Row {
            name: String::new(),
            description: "No skill — use built-in app detection".to_string(),
            is_auto: true,
        }];
        for s in skill::list_skills() {
            rows.push(Row {
                name: s.name,
                description: s.description,
                is_auto: false,
            });
        }
        self.rows = rows;
    }

    /// Index of the row matching the active skill (0 = Auto).
    fn active_row_index(&self) -> usize {
        match &self.active {
            None => 0,
            Some(name) => self
                .rows
                .iter()
                .position(|r| !r.is_auto && &r.name == name)
                .unwrap_or(0),
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.list_state.selected().and_then(|i| self.rows.get(i))
    }

    fn set_flash(&mut self, msg: impl Into<String>, ok: bool) {
        self.flash = Some(Flash {
            msg: msg.into(),
            until: Instant::now() + Duration::from_millis(2400),
            ok,
        });
    }

    /// Activate the selected row (Auto clears; a skill sets it). Persists.
    fn activate_selected(&mut self) {
        let Some(row) = self.selected() else { return };
        let (name_opt, label): (Option<String>, String) = if row.is_auto {
            (None, "Auto (built-in app detection)".to_string())
        } else {
            (Some(row.name.clone()), row.name.clone())
        };
        match skill::set_active(name_opt.as_deref()) {
            Ok(()) => {
                self.active = name_opt;
                self.set_flash(format!("Active skill: {label}"), true);
            }
            Err(e) => self.set_flash(format!("Failed to save: {e}"), false),
        }
    }
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
            handle_key(app, key);
        }
        if last.elapsed() >= tick {
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

fn handle_key(app: &mut App, key: KeyEvent) {
    if app.show_help {
        app.show_help = false;
        return;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('q'), _)
        | (KeyCode::Esc, _) => app.quit = true,
        (KeyCode::Char('?'), _) => app.show_help = true,
        (KeyCode::Char('r'), _) => {
            let sel = app.list_state.selected().unwrap_or(0);
            app.reload();
            app.list_state
                .select(Some(sel.min(app.rows.len().saturating_sub(1))));
            app.set_flash("Reloaded skills", true);
        }
        (KeyCode::Char('n'), _) => match skill::set_active(None) {
            Ok(()) => {
                app.active = None;
                app.set_flash("Active skill cleared (Auto)", true);
            }
            Err(e) => app.set_flash(format!("Failed to clear: {e}"), false),
        },
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => move_sel(app, -1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => move_sel(app, 1),
        (KeyCode::Enter, _) => app.activate_selected(),
        _ => {}
    }
}

fn move_sel(app: &mut App, delta: i32) {
    if app.rows.is_empty() {
        return;
    }
    let n = app.rows.len() as i32;
    let cur = app.list_state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).rem_euclid(n) as usize;
    app.list_state.select(Some(next));
}

// ── Rendering ───────────────────────────────────────────────────────────────

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(f, chunks[0], app);
    render_list(f, chunks[1], app);
    render_detail(f, chunks[2], app);
    render_footer(f, chunks[3], app);

    if app.show_help {
        render_help(f, area);
    }
}

fn render_header(f: &mut ratatui::Frame, area: Rect, app: &App) {
    // Cross-check the active name against the loaded rows so a deleted skill
    // shows as "missing" rather than falsely reading as active.
    let (active_label, label_color) = match &app.active {
        None => ("Auto".to_string(), OK),
        Some(name) => {
            let exists = app.rows.iter().any(|r| !r.is_auto && &r.name == name);
            if exists {
                (name.clone(), OK)
            } else {
                (format!("{name} (missing — using Auto)"), WARN)
            }
        }
    };
    let line = Line::from(vec![
        Span::styled(
            "  rekody skill",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   active: ", Style::default().fg(DIM)),
        Span::styled(
            active_label,
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_list(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let active = app.active.clone();
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let is_active = match &active {
                None => row.is_auto,
                Some(name) => !row.is_auto && &row.name == name,
            };
            let marker = if is_active { "● " } else { "  " };
            let marker_style = if is_active {
                Style::default().fg(OK)
            } else {
                Style::default().fg(SUBTLE)
            };
            let label = if row.is_auto {
                "Auto".to_string()
            } else {
                row.name.clone()
            };
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, marker_style),
                Span::styled(
                    format!("{label:<14}"),
                    Style::default().fg(FG_BOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(row.description.clone(), Style::default().fg(DIM)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BRAND_TEAL_LIGHT))
                .title(Span::styled(
                    " Skills ",
                    Style::default().fg(BRAND_TEAL_LIGHT),
                ))
                .padding(Padding::new(0, 0, 1, 0)),
        )
        .highlight_style(
            Style::default()
                .fg(FG_BOLD)
                .bg(BRAND_TEAL)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_detail(f: &mut ratatui::Frame, area: Rect, app: &App) {
    // When LLM post-processing is off, skills do nothing — say so up front
    // instead of letting the user pick one that silently never runs.
    if !app.llm_active {
        let p = Paragraph::new(Line::from(Span::styled(
            "  ⚠ LLM post-processing is off — skills won't take effect until you enable a provider (`rekody config`).",
            Style::default().fg(WARN),
        )))
        .wrap(Wrap { trim: true });
        f.render_widget(p, area);
        return;
    }
    let text = match app.selected() {
        Some(row) if row.is_auto => {
            "Selecting Auto clears the active skill — dictation uses the built-in per-app prompts."
                .to_string()
        }
        Some(row) => format!(
            "{} — press Enter to make it the active skill.",
            row.description
        ),
        None => String::new(),
    };
    let p = Paragraph::new(Line::from(Span::styled(
        format!("  {text}"),
        Style::default().fg(DIM),
    )))
    .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let mut spans = vec![
        chip("↑↓"),
        Span::raw(" nav  "),
        chip("⏎"),
        Span::raw(" activate  "),
        chip("n"),
        Span::raw(" clear  "),
        chip("r"),
        Span::raw(" reload  "),
        chip("?"),
        Span::raw(" help  "),
        chip("q"),
        Span::raw(" quit"),
    ];
    if let Some(flash) = &app.flash {
        spans.push(Span::raw("    "));
        let color = if flash.ok {
            OK
        } else {
            Color::Rgb(0xD9, 0x6B, 0x6B)
        };
        spans.push(Span::styled(
            flash.msg.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().fg(DIM)),
        area,
    );
}

fn chip(label: &str) -> Span<'_> {
    Span::styled(
        format!(" {label} "),
        Style::default().fg(FG_BOLD).bg(SUBTLE),
    )
}

fn render_help(f: &mut ratatui::Frame, area: Rect) {
    let modal = centered_rect(58, 52, area);
    f.render_widget(Clear, modal);
    let lines = vec![
        Line::from(Span::styled(
            "Skills",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  A skill reshapes your dictation via the LLM —",
            Style::default().fg(FG),
        )),
        Line::from(Span::styled(
            "  email, notes, spec, commit message, and more.",
            Style::default().fg(FG),
        )),
        Line::from(""),
        Line::from("  ↑/k, ↓/j   navigate"),
        Line::from("  Enter      activate selected (Auto = clear)"),
        Line::from("  n          clear active skill"),
        Line::from("  r          reload skills from disk"),
        Line::from("  ?          toggle this help"),
        Line::from("  q / Esc    quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  Skills are Markdown files in",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "  ~/.config/rekody/skills/ — add your own.",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "  An explicit pick overrides per-app triggers.",
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
