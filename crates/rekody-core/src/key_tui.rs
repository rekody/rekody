//! Interactive keychain manager for `rekody key`.
//!
//! ratatui list of known providers with their keychain status and a masked
//! preview of the stored key. Set/rotate via secure modal (input is masked
//! as you type), delete via confirm modal. Same visual language as
//! `history_tui.rs` (gold standard) and `config_tui.rs`.
//!
//! Keybindings:
//!   ↑/k, ↓/j    navigate
//!   Enter / s   set or rotate key for selected provider
//!   d           delete key for selected provider
//!   r           refresh (re-read keychain)
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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};

// ── Brand palette (duplicated from history_tui.rs intentionally) ────────────

const BRAND_TEAL: Color = Color::Rgb(0x20, 0x80, 0x8D);
const BRAND_TEAL_LIGHT: Color = Color::Rgb(0x4F, 0xB8, 0xC5);
const DIM: Color = Color::Rgb(0x77, 0x77, 0x77);
const SUBTLE: Color = Color::Rgb(0x55, 0x55, 0x55);
const FG: Color = Color::Rgb(0xE8, 0xE8, 0xE8);
const FG_BOLD: Color = Color::Rgb(0xFB, 0xFA, 0xF4);
const OK: Color = Color::Rgb(0x6B, 0xCB, 0x77);
const WARN: Color = Color::Rgb(0xE6, 0xB4, 0x50);
const ERR: Color = Color::Rgb(0xD9, 0x6B, 0x6B);

const SERVICE: &str = "com.rekody.voice";

/// Providers shown in the picker. Order matters — most-used first.
const PROVIDERS: &[&str] = &[
    "groq",
    "deepgram",
    "anthropic",
    "openai",
    "gemini",
    "cerebras",
    "together",
    "openrouter",
    "fireworks",
];

// ── Public entrypoint ───────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

// ── App state ───────────────────────────────────────────────────────────────

struct Row {
    provider: &'static str,
    masked: Option<String>, // None if no key stored
}

enum Modal {
    SetKey {
        provider: &'static str,
        buffer: String,
        error: Option<String>,
    },
    ConfirmDelete {
        provider: &'static str,
    },
    Help,
}

struct Flash {
    msg: String,
    until: Instant,
    ok: bool,
}

struct App {
    rows: Vec<Row>,
    list_state: ListState,
    modal: Option<Modal>,
    flash: Option<Flash>,
    quit: bool,
}

impl App {
    fn new() -> Self {
        let rows = load_rows();
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            rows,
            list_state,
            modal: None,
            flash: None,
            quit: false,
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.list_state.selected().and_then(|i| self.rows.get(i))
    }

    fn refresh(&mut self) {
        let sel = self.list_state.selected().unwrap_or(0);
        self.rows = load_rows();
        self.list_state
            .select(Some(sel.min(self.rows.len().saturating_sub(1))));
    }

    fn set_flash(&mut self, msg: impl Into<String>, ok: bool) {
        self.flash = Some(Flash {
            msg: msg.into(),
            until: Instant::now() + Duration::from_millis(2400),
            ok,
        });
    }
}

fn load_rows() -> Vec<Row> {
    PROVIDERS
        .iter()
        .map(|p| Row {
            provider: p,
            masked: read_key(p).ok().filter(|k| !k.is_empty()).map(|k| mask(&k)),
        })
        .collect()
}

// ── Keychain wrappers ───────────────────────────────────────────────────────

fn read_key(provider: &str) -> Result<String> {
    let entry = keyring::Entry::new(SERVICE, provider)?;
    Ok(entry.get_password()?)
}

fn write_key(provider: &str, key: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, provider)?;
    entry.set_password(key)?;
    Ok(())
}

fn delete_key(provider: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, provider)?;
    entry.delete_credential()?;
    Ok(())
}

fn mask(key: &str) -> String {
    if key.len() <= 4 {
        return "████".into();
    }
    let tail = &key[key.len() - 4..];
    format!("████████████{tail}")
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
    if app.modal.is_some() {
        handle_modal_key(app, key);
        return;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
        | (KeyCode::Char('q'), _)
        | (KeyCode::Esc, _) => app.quit = true,
        (KeyCode::Char('?'), _) => app.modal = Some(Modal::Help),
        (KeyCode::Char('r'), _) => {
            app.refresh();
            app.set_flash("Reloaded from keychain", true);
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => move_sel(app, -1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => move_sel(app, 1),
        (KeyCode::Enter, _) | (KeyCode::Char('s'), _) => {
            if let Some(row) = app.selected() {
                app.modal = Some(Modal::SetKey {
                    provider: row.provider,
                    buffer: String::new(),
                    error: None,
                });
            }
        }
        (KeyCode::Char('d'), _) => {
            if let Some(row) = app.selected()
                && row.masked.is_some()
            {
                app.modal = Some(Modal::ConfirmDelete {
                    provider: row.provider,
                });
            }
        }
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

fn handle_modal_key(app: &mut App, key: KeyEvent) {
    let mut close = false;
    let mut action: Option<KeyAction> = None;

    if let Some(modal) = app.modal.as_mut() {
        match (modal, key.code, key.modifiers) {
            (Modal::Help, _, _) => close = true,
            (Modal::ConfirmDelete { provider }, KeyCode::Char('y' | 'Y') | KeyCode::Enter, _) => {
                action = Some(KeyAction::Delete(provider));
            }
            (Modal::ConfirmDelete { .. }, _, _) => close = true,
            (Modal::SetKey { .. }, KeyCode::Esc, _)
            | (Modal::SetKey { .. }, KeyCode::Char('c'), KeyModifiers::CONTROL) => close = true,
            (
                Modal::SetKey {
                    provider, buffer, ..
                },
                KeyCode::Enter,
                _,
            ) => {
                if buffer.trim().is_empty() {
                    if let Modal::SetKey { error, .. } = app.modal.as_mut().unwrap() {
                        *error = Some("Key cannot be empty".into());
                    }
                } else {
                    action = Some(KeyAction::Set(provider, buffer.clone()));
                }
            }
            (Modal::SetKey { buffer, error, .. }, KeyCode::Backspace, _) => {
                buffer.pop();
                *error = None;
            }
            (Modal::SetKey { buffer, error, .. }, KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL) =>
            {
                buffer.push(c);
                *error = None;
            }
            _ => {}
        }
    }

    if let Some(act) = action {
        match act {
            KeyAction::Set(provider, key) => match write_key(provider, key.trim()) {
                Ok(()) => {
                    app.modal = None;
                    app.refresh();
                    app.set_flash(format!("Saved {provider} key"), true);
                }
                Err(e) => {
                    if let Some(Modal::SetKey { error, .. }) = app.modal.as_mut() {
                        *error = Some(format!("Keychain error: {e}"));
                    }
                }
            },
            KeyAction::Delete(provider) => {
                match delete_key(provider) {
                    Ok(()) => app.set_flash(format!("Deleted {provider} key"), true),
                    Err(_) => app.set_flash(format!("No {provider} key to delete"), false),
                }
                app.modal = None;
                app.refresh();
            }
        }
    } else if close {
        app.modal = None;
    }
}

enum KeyAction<'a> {
    Set(&'a str, String),
    Delete(&'a str),
}

// ── Rendering ───────────────────────────────────────────────────────────────

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(f, chunks[0]);
    render_list(f, chunks[1], app);
    render_footer(f, chunks[2], app);

    if let Some(modal) = &app.modal {
        match modal {
            Modal::SetKey {
                provider,
                buffer,
                error,
            } => render_set_modal(f, area, provider, buffer, error.as_deref()),
            Modal::ConfirmDelete { provider } => render_delete_modal(f, area, provider),
            Modal::Help => render_help(f, area),
        }
    }
}

fn render_header(f: &mut ratatui::Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            "  rekody keys",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  system keychain", Style::default().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_list(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let (status_text, status_style) = match &row.masked {
                Some(_) => ("✓ stored", Style::default().fg(OK)),
                None => ("—       ", Style::default().fg(DIM)),
            };
            let preview = row.masked.clone().unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<13}", row.provider),
                    Style::default().fg(FG_BOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{status_text}  "), status_style),
                Span::styled(preview, Style::default().fg(DIM)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BRAND_TEAL_LIGHT))
                .title(Span::styled(
                    " Providers ",
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

fn render_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let mut spans = vec![
        chip("↑↓"),
        Span::raw(" nav  "),
        chip("⏎"),
        Span::raw(" set/rotate  "),
        chip("d"),
        Span::raw(" delete  "),
        chip("r"),
        Span::raw(" reload  "),
        chip("?"),
        Span::raw(" help  "),
        chip("q"),
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

// ── Modals ──────────────────────────────────────────────────────────────────

fn render_set_modal(
    f: &mut ratatui::Frame,
    area: Rect,
    provider: &str,
    buffer: &str,
    error: Option<&str>,
) {
    let modal = centered_rect(60, 32, area);
    f.render_widget(Clear, modal);

    let masked: String = "●".repeat(buffer.chars().count());

    let mut lines = vec![
        Line::from(Span::styled(
            format!("  Enter API key for {provider}"),
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  Input is hidden as you type.",
            Style::default().fg(DIM),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{masked}_"),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("({} chars)", buffer.chars().count()),
                Style::default().fg(SUBTLE),
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
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ⏎ save   Esc cancel",
        Style::default().fg(DIM),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BRAND_TEAL_LIGHT))
        .title(Span::styled(
            " Set key ",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::new(1, 1, 1, 1));
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn render_delete_modal(f: &mut ratatui::Frame, area: Rect, provider: &str) {
    let modal = centered_rect(50, 22, area);
    f.render_widget(Clear, modal);
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Delete {provider} key from keychain?"),
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("  [y/N]", Style::default().fg(FG))),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(WARN))
        .title(Span::styled(
            " Confirm delete ",
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .padding(Padding::new(2, 2, 0, 0));
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn render_help(f: &mut ratatui::Frame, area: Rect) {
    let modal = centered_rect(54, 48, area);
    f.render_widget(Clear, modal);
    let lines = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default()
                .fg(BRAND_TEAL_LIGHT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  ↑/k, ↓/j   navigate"),
        Line::from("  Enter / s  set or rotate key"),
        Line::from("  d          delete (with confirm)"),
        Line::from("  r          reload from keychain"),
        Line::from("  ?          toggle this help"),
        Line::from("  q / Esc    quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Keys live in the system keychain under service",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "\"com.rekody.voice\" — `rekody key set/delete` are scriptable",
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            "equivalents for CI.",
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
