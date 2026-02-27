//! Terminal UI with vim-style editor, streaming output, and context bar

use agenticlaw_agent::{AgentConfig, AgentEvent, AgentRuntime, SessionKey};
use agenticlaw_tools::create_default_registry;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

// ---------------------------------------------------------------------------
// Vim mode
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum VimMode {
    Normal,
    Insert,
}

impl VimMode {
    fn label(&self) -> &str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
        }
    }

    fn color(&self) -> Color {
        match self {
            VimMode::Normal => Color::Blue,
            VimMode::Insert => Color::Green,
        }
    }
}

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Tracks what the agent is currently doing for spinner display.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentActivity {
    Idle,
    Thinking,
    Streaming,
    RunningTool(String),
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    // Editor
    pub mode: VimMode,
    pub editor_lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,

    // Output
    pub output_lines: Vec<String>,
    pub output_scroll: usize,

    // Agent state
    pub agent_running: bool,
    pub model: String,
    pub context_used: usize,
    pub context_max: usize,
    pub session_id: String,
    pub ctx_path: String,

    // Spinner state
    pub spinner_tick: usize,
    pub activity: AgentActivity,
    pub last_event_at: std::time::Instant,

    // Control
    pub should_quit: bool,
}

impl App {
    pub fn new(model: &str, session_id: &str, ctx_path: &str) -> Self {
        Self {
            mode: VimMode::Normal,
            editor_lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            output_lines: Vec::new(),
            output_scroll: 0,
            agent_running: false,
            model: model.to_string(),
            context_used: 0,
            context_max: 128_000,
            session_id: session_id.to_string(),
            ctx_path: ctx_path.to_string(),
            spinner_tick: 0,
            activity: AgentActivity::Idle,
            last_event_at: std::time::Instant::now(),
            should_quit: false,
        }
    }

    /// Advance the spinner frame counter. Called every ~100ms.
    pub fn tick_spinner(&mut self) {
        if self.agent_running {
            self.spinner_tick = self.spinner_tick.wrapping_add(1);
        }
    }

    pub fn spinner_char(&self) -> char {
        SPINNER_FRAMES[self.spinner_tick % SPINNER_FRAMES.len()]
    }

    pub fn activity_label(&self) -> String {
        if !self.agent_running {
            return String::new();
        }
        let elapsed = self.last_event_at.elapsed();
        if elapsed.as_secs() >= 30 {
            return format!("⚠ agent unresponsive ({}s)", elapsed.as_secs());
        }
        match &self.activity {
            AgentActivity::Idle => String::new(),
            AgentActivity::Thinking => "thinking...".to_string(),
            AgentActivity::Streaming => "streaming...".to_string(),
            AgentActivity::RunningTool(name) => format!("running {}...", name),
        }
    }

    fn editor_text(&self) -> String {
        self.editor_lines.join("\n")
    }

    fn clear_editor(&mut self) {
        self.editor_lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn push_output(&mut self, text: &str) {
        // Append text, handling newlines
        for ch in text.chars() {
            if ch == '\n' {
                self.output_lines.push(String::new());
            } else {
                if self.output_lines.is_empty() {
                    self.output_lines.push(String::new());
                }
                self.output_lines.last_mut().unwrap().push(ch);
            }
        }
        // Auto-scroll to keep bottom of output visible
        self.output_scroll = self.output_lines.len();
    }

    /// Number of characters in the current editor line.
    fn current_line_char_len(&self) -> usize {
        self.editor_lines[self.cursor_row].chars().count()
    }

    /// Convert a char-based cursor_col to a byte offset in the current line.
    fn cursor_byte_offset(&self) -> usize {
        char_to_byte(&self.editor_lines[self.cursor_row], self.cursor_col)
    }

    fn clamp_cursor(&mut self) {
        if self.cursor_row >= self.editor_lines.len() {
            self.cursor_row = self.editor_lines.len().saturating_sub(1);
        }
        let char_len = self.current_line_char_len();
        if self.mode == VimMode::Normal {
            self.cursor_col = self.cursor_col.min(char_len.saturating_sub(1).max(0));
        } else {
            self.cursor_col = self.cursor_col.min(char_len);
        }
    }
}

// ---------------------------------------------------------------------------
// UTF-8 helpers — cursor_col is a char index, Rust strings need byte offsets
// ---------------------------------------------------------------------------

/// Convert a character index to a byte offset in a string.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Returns Some(message) if the user wants to send a message.
fn handle_key(app: &mut App, key: KeyEvent) -> Option<String> {
    // Ctrl-C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return None;
    }

    match app.mode {
        VimMode::Normal => handle_normal_key(app, key),
        VimMode::Insert => handle_insert_key(app, key),
    }
}

fn handle_normal_key(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        // ESC cancels running agent (signaled via return value in main loop)
        KeyCode::Esc => None,

        // Enter sends the message
        KeyCode::Enter => {
            let text = app.editor_text();
            if !text.trim().is_empty() && !app.agent_running {
                app.push_output(&format!("\n> {}\n\n", text.trim()));
                app.clear_editor();
                return Some(text);
            }
            None
        }

        // Mode switches
        KeyCode::Char('i') => {
            app.mode = VimMode::Insert;
            None
        }
        KeyCode::Char('a') => {
            app.mode = VimMode::Insert;
            let char_len = app.current_line_char_len();
            app.cursor_col = (app.cursor_col + 1).min(char_len);
            None
        }
        KeyCode::Char('A') => {
            app.mode = VimMode::Insert;
            app.cursor_col = app.current_line_char_len();
            None
        }
        KeyCode::Char('I') => {
            app.mode = VimMode::Insert;
            app.cursor_col = 0;
            None
        }
        KeyCode::Char('o') => {
            app.mode = VimMode::Insert;
            let new_row = app.cursor_row + 1;
            app.editor_lines.insert(new_row, String::new());
            app.cursor_row = new_row;
            app.cursor_col = 0;
            None
        }

        // Movement
        KeyCode::Char('h') | KeyCode::Left => {
            app.cursor_col = app.cursor_col.saturating_sub(1);
            None
        }
        KeyCode::Char('l') | KeyCode::Right => {
            let max = app.current_line_char_len().saturating_sub(1);
            app.cursor_col = (app.cursor_col + 1).min(max);
            None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.cursor_row + 1 < app.editor_lines.len() {
                app.cursor_row += 1;
            }
            app.clamp_cursor();
            None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.cursor_row = app.cursor_row.saturating_sub(1);
            app.clamp_cursor();
            None
        }
        KeyCode::Char('0') => {
            app.cursor_col = 0;
            None
        }
        KeyCode::Char('$') => {
            app.cursor_col = app.current_line_char_len().saturating_sub(1).max(0);
            None
        }
        KeyCode::Char('w') => {
            // Jump to next word (char-index based)
            let line = &app.editor_lines[app.cursor_row];
            let chars: Vec<char> = line.chars().collect();
            let mut i = app.cursor_col;
            // skip current non-whitespace
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            // skip whitespace
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            app.cursor_col = i.min(chars.len().saturating_sub(1));
            None
        }
        KeyCode::Char('b') => {
            // Jump to previous word (char-index based)
            if app.cursor_col > 0 {
                let line = &app.editor_lines[app.cursor_row];
                let chars: Vec<char> = line.chars().collect();
                let mut i = app.cursor_col.saturating_sub(1);
                // skip whitespace backwards
                while i > 0 && chars[i].is_whitespace() {
                    i -= 1;
                }
                // skip non-whitespace backwards
                while i > 0 && !chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                app.cursor_col = i;
            }
            None
        }

        // Delete
        KeyCode::Char('x') => {
            let char_len = app.current_line_char_len();
            if char_len > 0 && app.cursor_col < char_len {
                let byte_off = app.cursor_byte_offset();
                let line = &mut app.editor_lines[app.cursor_row];
                line.remove(byte_off);
                app.clamp_cursor();
            }
            None
        }
        KeyCode::Char('d') => {
            // dd = delete line (simplified: always delete line on 'd')
            if app.editor_lines.len() > 1 {
                app.editor_lines.remove(app.cursor_row);
                app.clamp_cursor();
            } else {
                app.editor_lines[0].clear();
                app.cursor_col = 0;
            }
            None
        }

        // Scroll output
        KeyCode::Char('G') => {
            app.output_scroll = app.output_lines.len();
            None
        }
        KeyCode::Char('g') => {
            app.output_scroll = 0;
            None
        }

        // Quit
        KeyCode::Char('q') if !app.agent_running => {
            app.should_quit = true;
            None
        }

        _ => None,
    }
}

fn handle_insert_key(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Esc => {
            app.mode = VimMode::Normal;
            app.clamp_cursor();
            None
        }
        KeyCode::Enter => {
            // Split line at cursor (char-to-byte conversion)
            let byte_off = app.cursor_byte_offset();
            let rest = app.editor_lines[app.cursor_row][byte_off..].to_string();
            app.editor_lines[app.cursor_row].truncate(byte_off);
            app.cursor_row += 1;
            app.editor_lines.insert(app.cursor_row, rest);
            app.cursor_col = 0;
            None
        }
        KeyCode::Backspace => {
            if app.cursor_col > 0 {
                // Convert char position (cursor_col - 1) to byte offset
                let prev_byte = char_to_byte(&app.editor_lines[app.cursor_row], app.cursor_col - 1);
                app.editor_lines[app.cursor_row].remove(prev_byte);
                app.cursor_col -= 1;
            } else if app.cursor_row > 0 {
                let line = app.editor_lines.remove(app.cursor_row);
                app.cursor_row -= 1;
                app.cursor_col = app.editor_lines[app.cursor_row].chars().count();
                app.editor_lines[app.cursor_row].push_str(&line);
            }
            None
        }
        KeyCode::Char(c) => {
            let byte_off = app.cursor_byte_offset();
            app.editor_lines[app.cursor_row].insert(byte_off, c);
            app.cursor_col += 1;
            None
        }
        KeyCode::Left => {
            app.cursor_col = app.cursor_col.saturating_sub(1);
            None
        }
        KeyCode::Right => {
            let char_len = app.current_line_char_len();
            app.cursor_col = (app.cursor_col + 1).min(char_len);
            None
        }
        KeyCode::Up => {
            app.cursor_row = app.cursor_row.saturating_sub(1);
            app.clamp_cursor();
            None
        }
        KeyCode::Down => {
            if app.cursor_row + 1 < app.editor_lines.len() {
                app.cursor_row += 1;
            }
            app.clamp_cursor();
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Layout: output (3/4) | editor (1/4) | status (1 line)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),         // output
            Constraint::Percentage(25), // editor
            Constraint::Length(1),      // status bar
        ])
        .split(size);

    draw_output(frame, app, chunks[0]);
    draw_editor(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);
}

fn draw_output(frame: &mut Frame, app: &App, area: Rect) {
    let visible_height = area.height.saturating_sub(2) as usize; // subtract borders
    let inner_width = area.width.saturating_sub(2) as usize; // subtract borders

    // Convert all output lines to styled Lines
    let all_lines: Vec<Line> = app
        .output_lines
        .iter()
        .map(|l| {
            if l.starts_with("> ") {
                Line::from(Span::styled(l.as_str(), Style::default().fg(Color::Yellow)))
            } else if l.starts_with("[tool:") {
                Line::from(Span::styled(l.as_str(), Style::default().fg(Color::Cyan)))
            } else if l.starts_with("Error:") || l.starts_with("  error:") {
                Line::from(Span::styled(l.as_str(), Style::default().fg(Color::Red)))
            } else {
                Line::from(l.as_str())
            }
        })
        .collect();

    // When auto-scrolled (output_scroll == total), use scroll offset to pin to bottom.
    // Calculate total visual lines accounting for wrapping.
    let at_bottom = app.output_scroll >= app.output_lines.len();

    let title = if app.agent_running {
        " Output [running...] "
    } else {
        " Output "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if app.agent_running {
            Color::Yellow
        } else {
            Color::DarkGray
        }));

    if at_bottom && inner_width > 0 {
        // Count total visual lines after wrapping
        let total_visual: usize = all_lines
            .iter()
            .map(|l| {
                let w = l.width();
                if w == 0 {
                    1
                } else {
                    w.div_ceil(inner_width)
                }
            })
            .sum();

        let scroll_offset = total_visual.saturating_sub(visible_height) as u16;

        let paragraph = Paragraph::new(all_lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset, 0));

        frame.render_widget(paragraph, area);
    } else {
        // Manual scroll position: show from output_scroll backward
        let end = app.output_scroll.min(app.output_lines.len());
        let start = end.saturating_sub(visible_height);
        let visible: Vec<Line> = all_lines[start..end].to_vec();

        let paragraph = Paragraph::new(visible)
            .block(block)
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);
    }
}

fn draw_editor(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .editor_lines
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();

    let mode_label = app.mode.label();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", mode_label))
        .border_style(Style::default().fg(app.mode.color()));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);

    // Place cursor
    let cx = area.x + 1 + app.cursor_col as u16;
    let cy = area.y + 1 + app.cursor_row as u16;
    if cx < area.x + area.width - 1 && cy < area.y + area.height - 1 {
        frame.set_cursor_position((cx, cy));
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let ctx_pct = if app.context_max > 0 {
        ((app.context_used as f64 / app.context_max as f64) * 100.0).min(100.0) as u16
    } else {
        0
    };

    let ctx_color = if ctx_pct > 80 {
        Color::Red
    } else if ctx_pct > 50 {
        Color::Yellow
    } else {
        Color::Green
    };

    // Spinner (shown when agent is running)
    let spinner_span = if app.agent_running {
        let label = app.activity_label();
        let warn = label.starts_with('⚠');
        let color = if warn { Color::Red } else { Color::Yellow };
        Span::styled(
            format!(" {} {} ", app.spinner_char(), label),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    // Build status line as a gauge-like bar
    let mode_span = Span::styled(
        format!(" {} ", app.mode.label()),
        Style::default()
            .fg(Color::Black)
            .bg(app.mode.color())
            .add_modifier(Modifier::BOLD),
    );
    let model_span = Span::styled(
        format!(" {} ", app.model),
        Style::default().fg(Color::White).bg(Color::DarkGray),
    );
    let version_span = Span::styled(
        format!(" v{} ", env!("CARGO_PKG_VERSION")),
        Style::default().fg(Color::DarkGray).bg(Color::Black),
    );
    let session_span = Span::styled(
        format!(" {} ", app.session_id),
        Style::default().fg(Color::Gray).bg(Color::Black),
    );

    // Context bar
    let bar_width = area.width.saturating_sub(
        mode_span.width() as u16
            + model_span.width() as u16
            + version_span.width() as u16
            + session_span.width() as u16
            + spinner_span.width() as u16
            + 12,
    ) as usize;
    let filled = (bar_width as f64 * ctx_pct as f64 / 100.0) as usize;
    let empty = bar_width.saturating_sub(filled);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty),);
    let ctx_span = Span::styled(
        format!(" {}% {} ", ctx_pct, bar),
        Style::default().fg(ctx_color),
    );

    let status_line = Line::from(vec![
        mode_span,
        spinner_span,
        model_span,
        version_span,
        session_span,
        ctx_span,
    ]);
    let paragraph = Paragraph::new(status_line);
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Main TUI loop
// ---------------------------------------------------------------------------

pub async fn run_tui(
    workspace: Option<PathBuf>,
    session_name: Option<String>,
    model: Option<String>,
    resume: bool,
) -> anyhow::Result<()> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let workspace_root = workspace
        .or_else(|| std::env::var("RUSTCLAW_WORKSPACE").ok().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let default_model = model.unwrap_or_else(|| {
        std::env::var("RUSTCLAW_MODEL").unwrap_or_else(|_| "claude-opus-4-6".to_string())
    });

    let tools = create_default_registry(&workspace_root);
    let config = AgentConfig {
        default_model: default_model.clone(),
        max_tool_iterations: usize::MAX,
        system_prompt: None,
        workspace_root: workspace_root.clone(),
        sleep_threshold_pct: 1.0,
    };
    let runtime = Arc::new(AgentRuntime::new(&api_key, tools, config));

    // Resume or create session.
    // --session <name> ALWAYS resumes if a .ctx file exists (no separate --resume needed).
    // --resume without --session resumes the latest session.
    // Only creates a new session if no existing .ctx is found.
    let (session_key, ctx_path) = if let Some(ref name) = session_name {
        // Named session: stable path <name>.ctx — always the same file.
        let ctx_path = agenticlaw_agent::ctx_file::session_ctx_path(&workspace_root, name);
        let key = SessionKey::new(name);
        if ctx_path.exists() {
            let resumed = agenticlaw_agent::ctx_file::parse_for_resume(&ctx_path)?;
            runtime.sessions().resume_from_ctx(&resumed);
            tracing::info!("Resumed session '{}' from {}", name, ctx_path.display());
        } else {
            tracing::info!("Creating new session '{}'", name);
        }
        (key, ctx_path)
    } else if resume {
        // --resume without --session: resume latest
        let ctx = agenticlaw_agent::ctx_file::find_latest(&workspace_root).ok_or_else(|| {
            anyhow::anyhow!(
                "No .ctx files found to resume in {}",
                workspace_root.join(".agenticlaw/sessions").display()
            )
        })?;
        let resumed = agenticlaw_agent::ctx_file::parse_for_resume(&ctx)?;
        let key = SessionKey::new(&resumed.session_id);
        let path = resumed.ctx_path.clone();
        runtime.sessions().resume_from_ctx(&resumed);
        (key, path)
    } else {
        // No session name, no resume: fresh anonymous session
        let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let key = SessionKey::new(&session_id);
        let path = agenticlaw_agent::ctx_file::session_ctx_path(&workspace_root, &session_id);
        (key, path)
    };

    let session_id = session_key.as_str().to_string();
    let mut app = App::new(&default_model, &session_id, &ctx_path.to_string_lossy());

    // Setup terminal with panic hook to restore on crash
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Channels for agent events
    let (agent_event_tx, mut agent_event_rx) = mpsc::channel::<AgentEvent>(256);
    let (abort_tx, _abort_rx) = watch::channel(false);

    // Event loop
    let result = run_event_loop(
        &mut terminal,
        &mut app,
        runtime,
        session_key,
        &mut agent_event_rx,
        agent_event_tx,
        abort_tx,
    )
    .await;

    // Restore terminal
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    eprintln!("Agenticlaw complete, goodbye!");

    result
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    runtime: Arc<AgentRuntime>,
    session_key: SessionKey,
    agent_event_rx: &mut mpsc::Receiver<AgentEvent>,
    agent_event_tx: mpsc::Sender<AgentEvent>,
    abort_tx: watch::Sender<bool>,
) -> anyhow::Result<()> {
    let mut render_count: u64 = 0;

    loop {
        render_count = render_count.wrapping_add(1);
        if render_count.is_multiple_of(6) {
            app.tick_spinner();
        }

        // Draw
        terminal.draw(|f| draw(f, app))?;

        // Poll for events with short timeout so we can check agent events
        let timeout = std::time::Duration::from_millis(16); // ~60fps

        // Check terminal events
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                // ESC in normal mode while agent running = abort
                if key.code == KeyCode::Esc && app.mode == VimMode::Normal && app.agent_running {
                    let _ = abort_tx.send(true);
                    app.agent_running = false;
                    app.activity = AgentActivity::Idle;
                    app.push_output("\n[cancelled]\n");
                    continue;
                }

                if let Some(message) = handle_key(app, key) {
                    // Send message to agent
                    app.agent_running = true;
                    app.activity = AgentActivity::Thinking;
                    app.last_event_at = std::time::Instant::now();
                    let rt = runtime.clone();
                    let sk = session_key.clone();
                    let tx = agent_event_tx.clone();
                    let mut abort_rx = abort_tx.subscribe();

                    tokio::spawn(async move {
                        let turn = rt.run_turn(&sk, &message, tx);
                        tokio::select! {
                            result = turn => {
                                if let Err(e) = result {
                                    tracing::error!("Turn error: {}", e);
                                }
                            }
                            _ = async {
                                loop {
                                    abort_rx.changed().await.ok();
                                    if *abort_rx.borrow() { break; }
                                }
                            } => {
                                // Aborted
                            }
                        }
                    });

                    // Reset abort flag
                    let _ = abort_tx.send(false);
                }

                if app.should_quit {
                    break;
                }
            }
        }

        // Drain agent events
        while let Ok(event) = agent_event_rx.try_recv() {
            app.last_event_at = std::time::Instant::now();
            match event {
                AgentEvent::Text(text) => {
                    app.activity = AgentActivity::Streaming;
                    app.push_output(&text);
                }
                AgentEvent::Thinking(_) => {
                    app.activity = AgentActivity::Thinking;
                }
                AgentEvent::ToolCallStart { name, .. } => {
                    app.activity = AgentActivity::RunningTool(name.clone());
                    app.push_output(&format!("\n[tool:{}]\n", name));
                }
                AgentEvent::ToolExecuting { name, .. } => {
                    app.activity = AgentActivity::RunningTool(name.clone());
                    app.push_output(&format!("  executing {}...", name));
                }
                AgentEvent::ToolResult {
                    result, is_error, ..
                } => {
                    if is_error {
                        app.push_output(&format!(
                            "  error: {}\n",
                            &result[..result.len().min(200)]
                        ));
                    } else {
                        app.push_output(&format!("  done ({} chars)\n", result.len()));
                    }
                }
                AgentEvent::Done { .. } => {
                    app.push_output("\n");
                    app.agent_running = false;
                    app.activity = AgentActivity::Idle;
                    // Update context usage
                    if let Some(sess) = runtime.sessions().get(&session_key) {
                        app.context_used = sess.token_count().await;
                    }
                }
                AgentEvent::Error(e) => {
                    app.push_output(&format!("\nError: {}\n", e));
                    app.agent_running = false;
                    app.activity = AgentActivity::Idle;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_frames_cycle() {
        let mut app = App::new("test-model", "test-session", "/tmp/test.ctx");
        app.agent_running = true;

        assert_eq!(app.spinner_char(), SPINNER_FRAMES[0]);

        for i in 0..SPINNER_FRAMES.len() {
            app.spinner_tick = i;
            assert_eq!(app.spinner_char(), SPINNER_FRAMES[i]);
        }

        app.spinner_tick = SPINNER_FRAMES.len();
        assert_eq!(app.spinner_char(), SPINNER_FRAMES[0]);
    }

    #[test]
    fn activity_label_changes() {
        let mut app = App::new("test-model", "test-session", "/tmp/test.ctx");
        app.agent_running = true;

        app.activity = AgentActivity::Thinking;
        assert_eq!(app.activity_label(), "thinking...");

        app.activity = AgentActivity::Streaming;
        assert_eq!(app.activity_label(), "streaming...");

        app.activity = AgentActivity::RunningTool("bash".to_string());
        assert_eq!(app.activity_label(), "running bash...");

        app.agent_running = false;
        assert_eq!(app.activity_label(), "");
    }

    #[test]
    fn unresponsive_warning_after_30s() {
        let mut app = App::new("test-model", "test-session", "/tmp/test.ctx");
        app.agent_running = true;
        app.activity = AgentActivity::Thinking;
        app.last_event_at = std::time::Instant::now() - std::time::Duration::from_secs(31);
        let label = app.activity_label();
        assert!(
            label.contains('\u{26a0}'),
            "Expected warning, got: {}",
            label
        );
        assert!(
            label.contains("unresponsive"),
            "Expected unresponsive, got: {}",
            label
        );
    }

    #[test]
    fn tick_spinner_only_when_running() {
        let mut app = App::new("test-model", "test-session", "/tmp/test.ctx");
        assert_eq!(app.spinner_tick, 0);

        app.agent_running = false;
        app.tick_spinner();
        assert_eq!(app.spinner_tick, 0);

        app.agent_running = true;
        app.tick_spinner();
        assert_eq!(app.spinner_tick, 1);
    }
}
