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
    Visual,
    VisualLine,
    Search,
}

impl VimMode {
    fn label(&self) -> &str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "V-LINE",
            VimMode::Search => "SEARCH",
        }
    }

    fn color(&self) -> Color {
        match self {
            VimMode::Normal => Color::Blue,
            VimMode::Insert => Color::Green,
            VimMode::Visual | VimMode::VisualLine => Color::Magenta,
            VimMode::Search => Color::Yellow,
        }
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    // Editor
    pub mode: VimMode,
    pub vim_enabled: bool,
    pub editor_lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,

    // Pending key for multi-key commands (e.g., gg, dd)
    pub pending_key: Option<char>,

    // Visual mode anchor
    pub visual_anchor_row: usize,
    pub visual_anchor_col: usize,

    // Search
    pub search_query: String,
    pub search_forward: bool,
    pub search_matches: Vec<(usize, usize)>, // (line, col) pairs in output

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

    // Log panel
    pub show_logs: bool,
    pub log_lines: Vec<String>,
    pub log_scroll: usize,

    // Control
    pub should_quit: bool,
}

impl App {
    pub fn new(model: &str, session_id: &str, ctx_path: &str) -> Self {
        Self {
            mode: VimMode::Normal,
            vim_enabled: true,
            editor_lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            pending_key: None,
            visual_anchor_row: 0,
            visual_anchor_col: 0,
            search_query: String::new(),
            search_forward: true,
            search_matches: Vec::new(),
            output_lines: Vec::new(),
            output_scroll: 0,
            agent_running: false,
            model: model.to_string(),
            context_used: 0,
            context_max: 128_000,
            session_id: session_id.to_string(),
            ctx_path: ctx_path.to_string(),
            show_logs: false,
            log_lines: Vec::new(),
            log_scroll: 0,
            should_quit: false,
        }
    }

    pub fn push_log(&mut self, line: &str) {
        self.log_lines.push(line.to_string());
        if self.log_lines.len() > 1000 {
            self.log_lines.drain(0..100);
        }
        let visible = 10usize;
        if self.log_lines.len() > visible {
            self.log_scroll = self.log_lines.len() - visible;
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
        self.output_scroll = self.output_lines.len();
    }

    fn current_line_char_len(&self) -> usize {
        self.editor_lines[self.cursor_row].chars().count()
    }

    fn cursor_byte_offset(&self) -> usize {
        char_to_byte(&self.editor_lines[self.cursor_row], self.cursor_col)
    }

    fn clamp_cursor(&mut self) {
        if self.cursor_row >= self.editor_lines.len() {
            self.cursor_row = self.editor_lines.len().saturating_sub(1);
        }
        let char_len = self.current_line_char_len();
        if self.mode == VimMode::Normal
            || self.mode == VimMode::Visual
            || self.mode == VimMode::VisualLine
        {
            self.cursor_col = self.cursor_col.min(char_len.saturating_sub(1).max(0));
        } else {
            self.cursor_col = self.cursor_col.min(char_len);
        }
    }

    /// Perform search in output lines and populate matches.
    fn update_search_matches(&mut self) {
        self.search_matches.clear();
        if self.search_query.is_empty() {
            return;
        }
        let query = self.search_query.to_lowercase();
        for (line_idx, line) in self.output_lines.iter().enumerate() {
            let lower = line.to_lowercase();
            let mut start = 0;
            while let Some(pos) = lower[start..].find(&query) {
                self.search_matches.push((line_idx, start + pos));
                start += pos + 1;
            }
        }
    }

    /// Scroll output to bring a specific line into view.
    fn scroll_to_output_line(&mut self, line: usize) {
        self.output_scroll = line.saturating_add(1);
    }

    /// Find next search match from current output scroll position.
    fn find_next_match(&self) -> Option<usize> {
        if self.search_matches.is_empty() {
            return None;
        }
        let current_line = self.output_scroll;
        if self.search_forward {
            self.search_matches
                .iter()
                .position(|(l, _)| *l >= current_line)
                .or(Some(0))
        } else {
            self.search_matches
                .iter()
                .rposition(|(l, _)| *l < current_line)
                .or(Some(self.search_matches.len() - 1))
        }
    }

    /// Find previous search match from current output scroll position.
    fn find_prev_match(&self) -> Option<usize> {
        if self.search_matches.is_empty() {
            return None;
        }
        let current_line = self.output_scroll;
        if self.search_forward {
            self.search_matches
                .iter()
                .rposition(|(l, _)| *l < current_line)
                .or(Some(self.search_matches.len() - 1))
        } else {
            self.search_matches
                .iter()
                .position(|(l, _)| *l >= current_line)
                .or(Some(0))
        }
    }
}

// ---------------------------------------------------------------------------
// UTF-8 helpers
// ---------------------------------------------------------------------------

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Word motion helpers
// ---------------------------------------------------------------------------

/// Find the char index of the next word start (vim `w`).
fn word_forward(chars: &[char], from: usize) -> usize {
    let len = chars.len();
    if from >= len {
        return len.saturating_sub(1);
    }
    let mut i = from;
    // Skip current word (non-whitespace)
    while i < len && !chars[i].is_whitespace() {
        i += 1;
    }
    // Skip whitespace
    while i < len && chars[i].is_whitespace() {
        i += 1;
    }
    i.min(len.saturating_sub(1))
}

/// Find the char index of the previous word start (vim `b`).
fn word_backward(chars: &[char], from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    let mut i = from.saturating_sub(1);
    // Skip whitespace backwards
    while i > 0 && chars[i].is_whitespace() {
        i -= 1;
    }
    // Skip non-whitespace backwards
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// Find the char index of the end of the current/next word (vim `e`).
fn word_end(chars: &[char], from: usize) -> usize {
    let len = chars.len();
    if from >= len.saturating_sub(1) {
        return len.saturating_sub(1);
    }
    let mut i = from + 1;
    // Skip whitespace
    while i < len && chars[i].is_whitespace() {
        i += 1;
    }
    // Move to end of word
    while i + 1 < len && !chars[i + 1].is_whitespace() {
        i += 1;
    }
    i.min(len.saturating_sub(1))
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

fn handle_key(app: &mut App, key: KeyEvent) -> Option<String> {
    // Ctrl-C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return None;
    }

    // Ctrl-L toggles log panel
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
        app.show_logs = !app.show_logs;
        return None;
    }

    // Ctrl+Shift+V toggles vim mode on/off
    if key
        .modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        && key.code == KeyCode::Char('V')
    {
        app.vim_enabled = !app.vim_enabled;
        if app.vim_enabled {
            app.mode = VimMode::Normal;
        } else {
            app.mode = VimMode::Insert;
        }
        app.pending_key = None;
        return None;
    }

    if !app.vim_enabled {
        return handle_plain_key(app, key);
    }

    match app.mode {
        VimMode::Normal => handle_normal_key(app, key),
        VimMode::Insert => handle_insert_key(app, key),
        VimMode::Visual | VimMode::VisualLine => handle_visual_key(app, key),
        VimMode::Search => handle_search_key(app, key),
    }
}

/// Handle keys when vim mode is disabled (plain editor).
fn handle_plain_key(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                // Shift+Enter: newline
                let byte_off = app.cursor_byte_offset();
                let rest = app.editor_lines[app.cursor_row][byte_off..].to_string();
                app.editor_lines[app.cursor_row].truncate(byte_off);
                app.cursor_row += 1;
                app.editor_lines.insert(app.cursor_row, rest);
                app.cursor_col = 0;
                None
            } else {
                let text = app.editor_text();
                if !text.trim().is_empty() && !app.agent_running {
                    app.push_output(&format!("\n> {}\n\n", text.trim()));
                    app.clear_editor();
                    return Some(text);
                }
                None
            }
        }
        KeyCode::Backspace => {
            if app.cursor_col > 0 {
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

fn handle_normal_key(app: &mut App, key: KeyEvent) -> Option<String> {
    // Handle pending multi-key commands
    if let Some(pending) = app.pending_key.take() {
        return handle_pending_key(app, pending, key);
    }

    // Ctrl+D: half-page down (output scroll)
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
        app.output_scroll = (app.output_scroll + 10).min(app.output_lines.len());
        return None;
    }
    // Ctrl+U: half-page up (output scroll)
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
        app.output_scroll = app.output_scroll.saturating_sub(10);
        return None;
    }
    // Ctrl+F: full page down
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
        app.output_scroll = (app.output_scroll + 20).min(app.output_lines.len());
        return None;
    }
    // Ctrl+B: full page back
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
        app.output_scroll = app.output_scroll.saturating_sub(20);
        return None;
    }

    match key.code {
        KeyCode::Esc => None,

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
            // Move to first non-whitespace character
            let line = &app.editor_lines[app.cursor_row];
            let first_non_ws = line.chars().position(|c| !c.is_whitespace()).unwrap_or(0);
            app.cursor_col = first_non_ws;
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
        KeyCode::Char('O') => {
            app.mode = VimMode::Insert;
            app.editor_lines.insert(app.cursor_row, String::new());
            app.cursor_col = 0;
            None
        }

        // Visual modes
        KeyCode::Char('v') => {
            app.mode = VimMode::Visual;
            app.visual_anchor_row = app.cursor_row;
            app.visual_anchor_col = app.cursor_col;
            None
        }
        KeyCode::Char('V') => {
            app.mode = VimMode::VisualLine;
            app.visual_anchor_row = app.cursor_row;
            app.visual_anchor_col = 0;
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
        KeyCode::Char('^') => {
            // First non-whitespace char
            let line = &app.editor_lines[app.cursor_row];
            app.cursor_col = line.chars().position(|c| !c.is_whitespace()).unwrap_or(0);
            None
        }
        KeyCode::Char('$') => {
            app.cursor_col = app.current_line_char_len().saturating_sub(1).max(0);
            None
        }
        KeyCode::Char('w') => {
            let chars: Vec<char> = app.editor_lines[app.cursor_row].chars().collect();
            app.cursor_col = word_forward(&chars, app.cursor_col);
            None
        }
        KeyCode::Char('b') => {
            let chars: Vec<char> = app.editor_lines[app.cursor_row].chars().collect();
            app.cursor_col = word_backward(&chars, app.cursor_col);
            None
        }
        KeyCode::Char('e') => {
            let chars: Vec<char> = app.editor_lines[app.cursor_row].chars().collect();
            app.cursor_col = word_end(&chars, app.cursor_col);
            None
        }

        // gg/G — output scroll + editor navigation
        KeyCode::Char('g') => {
            app.pending_key = Some('g');
            None
        }
        KeyCode::Char('G') => {
            // Go to last line of editor, also scroll output to bottom
            app.output_scroll = app.output_lines.len();
            app.cursor_row = app.editor_lines.len().saturating_sub(1);
            app.clamp_cursor();
            None
        }

        // Paragraph movement (editor)
        KeyCode::Char('{') => {
            // Move up to previous blank line
            let mut row = app.cursor_row;
            row = row.saturating_sub(1);
            while row > 0 && !app.editor_lines[row].trim().is_empty() {
                row -= 1;
            }
            app.cursor_row = row;
            app.cursor_col = 0;
            None
        }
        KeyCode::Char('}') => {
            // Move down to next blank line
            let mut row = app.cursor_row;
            let max = app.editor_lines.len().saturating_sub(1);
            if row < max {
                row += 1;
            }
            while row < max && !app.editor_lines[row].trim().is_empty() {
                row += 1;
            }
            app.cursor_row = row;
            app.cursor_col = 0;
            None
        }

        // Delete
        KeyCode::Char('x') => {
            let char_len = app.current_line_char_len();
            if char_len > 0 && app.cursor_col < char_len {
                let byte_off = app.cursor_byte_offset();
                app.editor_lines[app.cursor_row].remove(byte_off);
                app.clamp_cursor();
            }
            None
        }
        KeyCode::Char('d') => {
            app.pending_key = Some('d');
            None
        }
        KeyCode::Char('y') => {
            app.pending_key = Some('y');
            None
        }

        // Search
        KeyCode::Char('/') => {
            app.mode = VimMode::Search;
            app.search_query.clear();
            app.search_forward = true;
            None
        }
        KeyCode::Char('?') => {
            app.mode = VimMode::Search;
            app.search_query.clear();
            app.search_forward = false;
            None
        }
        KeyCode::Char('n') => {
            if let Some(idx) = app.find_next_match() {
                let (line, _) = app.search_matches[idx];
                app.scroll_to_output_line(line);
            }
            None
        }
        KeyCode::Char('N') => {
            if let Some(idx) = app.find_prev_match() {
                let (line, _) = app.search_matches[idx];
                app.scroll_to_output_line(line);
            }
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

/// Handle second key of multi-key normal mode commands.
fn handle_pending_key(app: &mut App, pending: char, key: KeyEvent) -> Option<String> {
    match (pending, key.code) {
        ('g', KeyCode::Char('g')) => {
            // gg: go to top of editor, scroll output to top
            app.output_scroll = 0;
            app.cursor_row = 0;
            app.cursor_col = 0;
            None
        }
        ('d', KeyCode::Char('d')) => {
            // dd: delete current line
            if app.editor_lines.len() > 1 {
                app.editor_lines.remove(app.cursor_row);
                app.clamp_cursor();
            } else {
                app.editor_lines[0].clear();
                app.cursor_col = 0;
            }
            None
        }
        ('d', KeyCode::Char('w')) => {
            // dw: delete to next word
            let line = &app.editor_lines[app.cursor_row];
            let chars: Vec<char> = line.chars().collect();
            let end = word_forward(&chars, app.cursor_col);
            let start_byte = char_to_byte(line, app.cursor_col);
            let end_byte = char_to_byte(line, end);
            app.editor_lines[app.cursor_row] =
                format!("{}{}", &line[..start_byte], &line[end_byte..]);
            app.clamp_cursor();
            None
        }
        ('d', KeyCode::Char('$')) => {
            // d$: delete to end of line
            let byte_off = app.cursor_byte_offset();
            app.editor_lines[app.cursor_row].truncate(byte_off);
            app.clamp_cursor();
            None
        }
        ('d', KeyCode::Char('0')) => {
            // d0: delete to start of line
            let byte_off = app.cursor_byte_offset();
            let line = &app.editor_lines[app.cursor_row];
            app.editor_lines[app.cursor_row] = line[byte_off..].to_string();
            app.cursor_col = 0;
            None
        }
        ('y', KeyCode::Char('y')) => {
            // yy: yank line (no clipboard in TUI, just acknowledge)
            None
        }
        _ => {
            // Unknown sequence, ignore
            None
        }
    }
}

fn handle_visual_key(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Esc => {
            app.mode = VimMode::Normal;
            app.clamp_cursor();
            None
        }
        // Movement (same as normal mode)
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
        KeyCode::Char('w') => {
            let chars: Vec<char> = app.editor_lines[app.cursor_row].chars().collect();
            app.cursor_col = word_forward(&chars, app.cursor_col);
            None
        }
        KeyCode::Char('b') => {
            let chars: Vec<char> = app.editor_lines[app.cursor_row].chars().collect();
            app.cursor_col = word_backward(&chars, app.cursor_col);
            None
        }
        KeyCode::Char('e') => {
            let chars: Vec<char> = app.editor_lines[app.cursor_row].chars().collect();
            app.cursor_col = word_end(&chars, app.cursor_col);
            None
        }
        KeyCode::Char('$') => {
            app.cursor_col = app.current_line_char_len().saturating_sub(1).max(0);
            None
        }
        KeyCode::Char('0') => {
            app.cursor_col = 0;
            None
        }
        // Delete selection
        KeyCode::Char('d') | KeyCode::Char('x') => {
            delete_visual_selection(app);
            app.mode = VimMode::Normal;
            app.clamp_cursor();
            None
        }
        // Switch between visual modes
        KeyCode::Char('v') => {
            if app.mode == VimMode::Visual {
                app.mode = VimMode::Normal;
            } else {
                app.mode = VimMode::Visual;
            }
            None
        }
        KeyCode::Char('V') => {
            if app.mode == VimMode::VisualLine {
                app.mode = VimMode::Normal;
            } else {
                app.mode = VimMode::VisualLine;
            }
            None
        }
        _ => None,
    }
}

fn handle_search_key(app: &mut App, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Esc => {
            app.mode = VimMode::Normal;
            app.search_query.clear();
            None
        }
        KeyCode::Enter => {
            app.update_search_matches();
            if let Some(idx) = app.find_next_match() {
                let (line, _) = app.search_matches[idx];
                app.scroll_to_output_line(line);
            }
            app.mode = VimMode::Normal;
            None
        }
        KeyCode::Backspace => {
            app.search_query.pop();
            None
        }
        KeyCode::Char(c) => {
            app.search_query.push(c);
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

/// Delete the visual selection from the editor.
fn delete_visual_selection(app: &mut App) {
    let (start_row, start_col, end_row, end_col) = if app.mode == VimMode::VisualLine {
        let sr = app.visual_anchor_row.min(app.cursor_row);
        let er = app.visual_anchor_row.max(app.cursor_row);
        (sr, 0, er, usize::MAX)
    } else {
        let (sr, sc, er, ec) =
            if (app.cursor_row, app.cursor_col) < (app.visual_anchor_row, app.visual_anchor_col) {
                (
                    app.cursor_row,
                    app.cursor_col,
                    app.visual_anchor_row,
                    app.visual_anchor_col,
                )
            } else {
                (
                    app.visual_anchor_row,
                    app.visual_anchor_col,
                    app.cursor_row,
                    app.cursor_col,
                )
            };
        (sr, sc, er, ec)
    };

    if app.mode == VimMode::VisualLine {
        // Delete entire lines
        let count = end_row - start_row + 1;
        for _ in 0..count {
            if app.editor_lines.len() > 1 {
                app.editor_lines.remove(start_row);
            } else {
                app.editor_lines[0].clear();
            }
        }
        app.cursor_row = start_row.min(app.editor_lines.len().saturating_sub(1));
        app.cursor_col = 0;
    } else if start_row == end_row {
        // Single line selection
        let line = &app.editor_lines[start_row];
        let sb = char_to_byte(line, start_col);
        let eb = char_to_byte(line, (end_col + 1).min(line.chars().count()));
        app.editor_lines[start_row] = format!("{}{}", &line[..sb], &line[eb..]);
        app.cursor_row = start_row;
        app.cursor_col = start_col;
    } else {
        // Multi-line selection
        let first_line = &app.editor_lines[start_row];
        let sb = char_to_byte(first_line, start_col);
        let prefix = first_line[..sb].to_string();

        let last_line = &app.editor_lines[end_row];
        let eb = char_to_byte(last_line, (end_col + 1).min(last_line.chars().count()));
        let suffix = last_line[eb..].to_string();

        // Remove intermediate lines
        let count = end_row - start_row + 1;
        for _ in 0..count {
            app.editor_lines.remove(start_row);
        }
        app.editor_lines
            .insert(start_row, format!("{}{}", prefix, suffix));
        app.cursor_row = start_row;
        app.cursor_col = start_col;
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    if app.show_logs {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Percentage(20),
                Constraint::Percentage(20),
                Constraint::Length(1),
            ])
            .split(size);

        draw_output(frame, app, chunks[0]);
        draw_log_panel(frame, app, chunks[1]);
        draw_editor(frame, app, chunks[2]);
        draw_status(frame, app, chunks[3]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Percentage(25),
                Constraint::Length(1),
            ])
            .split(size);

        draw_output(frame, app, chunks[0]);
        draw_editor(frame, app, chunks[1]);
        draw_status(frame, app, chunks[2]);
    }
}

fn draw_log_panel(frame: &mut Frame, app: &App, area: Rect) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .log_lines
        .iter()
        .skip(app.log_scroll)
        .take(visible_height)
        .map(|l| {
            Line::from(Span::styled(
                l.as_str(),
                Style::default().fg(Color::DarkGray),
            ))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Logs (Ctrl+L to hide) ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_output(frame: &mut Frame, app: &App, area: Rect) {
    let visible_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;

    let search_query = if !app.search_query.is_empty() {
        Some(app.search_query.to_lowercase())
    } else {
        None
    };

    let all_lines: Vec<Line> = app
        .output_lines
        .iter()
        .map(|l| {
            // Highlight search matches
            if let Some(ref query) = search_query {
                let lower = l.to_lowercase();
                if lower.contains(query) {
                    return Line::from(Span::styled(
                        l.as_str(),
                        Style::default().fg(Color::Black).bg(Color::Yellow),
                    ));
                }
            }

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

    let at_bottom = app.output_scroll >= app.output_lines.len();

    let title = if app.mode == VimMode::Search {
        format!(
            " Output [{}{}] ",
            if app.search_forward { "/" } else { "?" },
            app.search_query
        )
    } else if app.agent_running {
        " Output [running...] ".to_string()
    } else {
        " Output ".to_string()
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

    let mode_label = if app.vim_enabled {
        app.mode.label()
    } else {
        "PLAIN"
    };
    let mode_color = if app.vim_enabled {
        app.mode.color()
    } else {
        Color::White
    };

    let title = if let Some(pending) = app.pending_key {
        format!(" {} [{}…] ", mode_label, pending)
    } else {
        format!(" {} ", mode_label)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(mode_color));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
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

    let mode_label = if app.vim_enabled {
        app.mode.label()
    } else {
        "PLAIN"
    };
    let mode_color = if app.vim_enabled {
        app.mode.color()
    } else {
        Color::White
    };

    let mode_span = Span::styled(
        format!(" {} ", mode_label),
        Style::default()
            .fg(Color::Black)
            .bg(mode_color)
            .add_modifier(Modifier::BOLD),
    );
    let model_span = Span::styled(
        format!(" {} ", app.model),
        Style::default().fg(Color::White).bg(Color::DarkGray),
    );
    let session_span = Span::styled(
        format!(" {} ", app.session_id),
        Style::default().fg(Color::Gray).bg(Color::Black),
    );

    // Vim toggle hint
    let vim_hint = if app.vim_enabled {
        Span::styled(" Ctrl+Shift+V:plain ", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(" Ctrl+Shift+V:vim ", Style::default().fg(Color::DarkGray))
    };

    let bar_width = area.width.saturating_sub(
        mode_span.width() as u16
            + model_span.width() as u16
            + session_span.width() as u16
            + vim_hint.width() as u16
            + 12,
    ) as usize;
    let filled = (bar_width as f64 * ctx_pct as f64 / 100.0) as usize;
    let empty = bar_width.saturating_sub(filled);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    let ctx_span = Span::styled(
        format!(" {}% {} ", ctx_pct, bar),
        Style::default().fg(ctx_color),
    );

    let status_line = Line::from(vec![
        mode_span,
        model_span,
        session_span,
        ctx_span,
        vim_hint,
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

    let (session_key, ctx_path) = if let Some(ref name) = session_name {
        let key = SessionKey::new(name);
        if let Some(latest) = agenticlaw_agent::ctx_file::find_by_id(&workspace_root, name) {
            let resumed = agenticlaw_agent::ctx_file::parse_for_resume(&latest)?;
            runtime.sessions().resume_from_ctx(&resumed);
            tracing::info!("Resumed session '{}' from {}", name, latest.display());
            (key, latest)
        } else {
            let ctx_path = agenticlaw_agent::ctx_file::session_ctx_path(&workspace_root, name);
            tracing::info!("Creating new session '{}'", name);
            (key, ctx_path)
        }
    } else if resume {
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
        let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let key = SessionKey::new(&session_id);
        let path = agenticlaw_agent::ctx_file::session_ctx_path(&workspace_root, &session_id);
        (key, path)
    };

    let session_id = session_key.as_str().to_string();
    let mut app = App::new(&default_model, &session_id, &ctx_path.to_string_lossy());

    if ctx_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&ctx_path) {
            app.push_output(&content);
        }
    }

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

    let (agent_event_tx, mut agent_event_rx) = mpsc::channel::<AgentEvent>(256);
    let (abort_tx, _abort_rx) = watch::channel(false);

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
    loop {
        terminal.draw(|f| draw(f, app))?;

        let timeout = std::time::Duration::from_millis(16);

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Esc
                    && app.mode == VimMode::Normal
                    && app.agent_running
                    && app.vim_enabled
                {
                    let _ = abort_tx.send(true);
                    app.agent_running = false;
                    app.push_output("\n[cancelled]\n");
                    continue;
                }

                if let Some(message) = handle_key(app, key) {
                    app.agent_running = true;
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

                    let _ = abort_tx.send(false);
                }

                if app.should_quit {
                    break;
                }
            }
        }

        while let Ok(event) = agent_event_rx.try_recv() {
            match event {
                AgentEvent::Text(text) => app.push_output(&text),
                AgentEvent::Thinking(_) => {}
                AgentEvent::ToolCallStart { name, .. } => {
                    app.push_output(&format!("\n[tool:{}]\n", name));
                }
                AgentEvent::ToolExecuting { name, .. } => {
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
                    if let Some(sess) = runtime.sessions().get(&session_key) {
                        app.context_used = sess.token_count().await;
                    }
                }
                AgentEvent::Error(e) => {
                    app.push_output(&format!("\nError: {}\n", e));
                    app.agent_running = false;
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

    // UTF-8 helpers

    #[test]
    fn char_to_byte_ascii() {
        assert_eq!(char_to_byte("hello", 0), 0);
        assert_eq!(char_to_byte("hello", 3), 3);
        assert_eq!(char_to_byte("hello", 5), 5);
    }

    #[test]
    fn char_to_byte_multibyte() {
        let s = "hi🦀bye";
        // 'h'=0, 'i'=1, '🦀'=2(bytes 2-5), 'b'=3(byte 6), 'y'=4(byte 7), 'e'=5(byte 8)
        assert_eq!(char_to_byte(s, 0), 0);
        assert_eq!(char_to_byte(s, 2), 2); // start of crab
        assert_eq!(char_to_byte(s, 3), 6); // 'b' after crab
    }

    // Word motions

    #[test]
    fn word_forward_basic() {
        let chars: Vec<char> = "hello world foo".chars().collect();
        assert_eq!(word_forward(&chars, 0), 6); // -> 'w'
        assert_eq!(word_forward(&chars, 6), 12); // -> 'f'
    }

    #[test]
    fn word_forward_at_end() {
        let chars: Vec<char> = "hello".chars().collect();
        assert_eq!(word_forward(&chars, 4), 4);
    }

    #[test]
    fn word_backward_basic() {
        let chars: Vec<char> = "hello world foo".chars().collect();
        assert_eq!(word_backward(&chars, 12), 6); // 'f' -> 'w'
        assert_eq!(word_backward(&chars, 6), 0); // 'w' -> 'h'
    }

    #[test]
    fn word_backward_at_start() {
        let chars: Vec<char> = "hello".chars().collect();
        assert_eq!(word_backward(&chars, 0), 0);
    }

    #[test]
    fn word_end_basic() {
        let chars: Vec<char> = "hello world foo".chars().collect();
        assert_eq!(word_end(&chars, 0), 4); // -> 'o' of hello
        assert_eq!(word_end(&chars, 4), 10); // -> 'd' of world
    }

    #[test]
    fn word_end_at_end() {
        let chars: Vec<char> = "hi".chars().collect();
        assert_eq!(word_end(&chars, 1), 1);
    }

    // App state

    #[test]
    fn app_clear_editor() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.editor_lines = vec!["hello".to_string(), "world".to_string()];
        app.cursor_row = 1;
        app.cursor_col = 3;
        app.clear_editor();
        assert_eq!(app.editor_lines, vec![""]);
        assert_eq!(app.cursor_row, 0);
        assert_eq!(app.cursor_col, 0);
    }

    #[test]
    fn app_editor_text_multiline() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.editor_lines = vec!["line1".to_string(), "line2".to_string()];
        assert_eq!(app.editor_text(), "line1\nline2");
    }

    #[test]
    fn app_push_output_newlines() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.push_output("hello\nworld\n");
        assert_eq!(app.output_lines, vec!["hello", "world", ""]);
    }

    #[test]
    fn clamp_cursor_normal_mode() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.mode = VimMode::Normal;
        app.editor_lines = vec!["hi".to_string()];
        app.cursor_col = 5;
        app.clamp_cursor();
        assert_eq!(app.cursor_col, 1); // max is len-1 in normal
    }

    #[test]
    fn clamp_cursor_insert_mode() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.mode = VimMode::Insert;
        app.editor_lines = vec!["hi".to_string()];
        app.cursor_col = 5;
        app.clamp_cursor();
        assert_eq!(app.cursor_col, 2); // can be at end in insert
    }

    // Vim mode toggle

    #[test]
    fn vim_mode_labels() {
        assert_eq!(VimMode::Normal.label(), "NORMAL");
        assert_eq!(VimMode::Insert.label(), "INSERT");
        assert_eq!(VimMode::Visual.label(), "VISUAL");
        assert_eq!(VimMode::VisualLine.label(), "V-LINE");
        assert_eq!(VimMode::Search.label(), "SEARCH");
    }

    // Search

    #[test]
    fn search_matches_case_insensitive() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.output_lines = vec![
            "Hello World".to_string(),
            "goodbye".to_string(),
            "HELLO again".to_string(),
        ];
        app.search_query = "hello".to_string();
        app.update_search_matches();
        assert_eq!(app.search_matches.len(), 2);
        assert_eq!(app.search_matches[0], (0, 0));
        assert_eq!(app.search_matches[1], (2, 0));
    }

    #[test]
    fn search_no_matches() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.output_lines = vec!["hello".to_string()];
        app.search_query = "xyz".to_string();
        app.update_search_matches();
        assert!(app.search_matches.is_empty());
    }

    // Visual selection delete

    #[test]
    fn delete_visual_single_line() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.mode = VimMode::Visual;
        app.editor_lines = vec!["hello world".to_string()];
        app.visual_anchor_row = 0;
        app.visual_anchor_col = 0;
        app.cursor_row = 0;
        app.cursor_col = 4; // select "hello"
        delete_visual_selection(&mut app);
        assert_eq!(app.editor_lines[0], " world");
    }

    #[test]
    fn delete_visual_line_mode() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.mode = VimMode::VisualLine;
        app.editor_lines = vec![
            "line1".to_string(),
            "line2".to_string(),
            "line3".to_string(),
        ];
        app.visual_anchor_row = 0;
        app.cursor_row = 1;
        delete_visual_selection(&mut app);
        assert_eq!(app.editor_lines, vec!["line3"]);
    }

    // Pending key

    #[test]
    fn pending_key_gg_resets() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.cursor_row = 2;
        app.editor_lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        app.output_scroll = 50;
        app.pending_key = Some('g');
        let result = handle_pending_key(
            &mut app,
            'g',
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert!(result.is_none());
        assert_eq!(app.cursor_row, 0);
        assert_eq!(app.output_scroll, 0);
    }

    #[test]
    fn pending_key_dd_deletes_line() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.editor_lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        app.cursor_row = 1;
        app.pending_key = Some('d');
        handle_pending_key(
            &mut app,
            'd',
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(app.editor_lines, vec!["a", "c"]);
    }

    #[test]
    fn pending_key_unknown_is_noop() {
        let mut app = App::new("test", "s1", "/tmp/test.ctx");
        app.editor_lines = vec!["hello".to_string()];
        let result = handle_pending_key(
            &mut app,
            'z',
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert!(result.is_none());
        assert_eq!(app.editor_lines, vec!["hello"]);
    }
}
