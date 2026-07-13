//! Modal editor: selection-first editing over a rope buffer with
//! tree-sitter highlighting. A deliberately small state machine of
//! Normal/Insert/Select modes plus a `:` command line.

pub mod highlight;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ropey::Rope;
use std::path::{Path, PathBuf};

use highlight::HighlightSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Select,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Select => "SELECT",
        }
    }
}

/// What the app should do after a key was handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    None,
    /// `:q` / `:wq` — close the editor pane.
    Close,
    /// `space k` — request LSP hover for the cursor position.
    Hover,
    /// `gd` / ctrl+click — jump to the symbol's definition.
    GotoDefinition,
    /// Ctrl+O — jump back to where we came from.
    JumpBack,
    /// Esc with nothing to cancel — hand focus back to the app.
    FocusOut,
    /// The buffer was written (`:w` / `:wq`) — notify the language server.
    Saved,
}

#[derive(Clone)]
struct Snapshot {
    text: Rope,
    anchor: usize,
    head: usize,
}

enum CharClass {
    Word,
    Punct,
    Space,
}

fn class_of(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Punct
    }
}

pub struct Editor {
    pub path: PathBuf,
    pub text: Rope,
    pub mode: Mode,
    /// Selection endpoints as char indices; `head` carries the cursor.
    /// In Normal/Select the cursor sits ON the char at `head` (inclusive
    /// range with `anchor`); in Insert `head` is the insertion point.
    pub anchor: usize,
    pub head: usize,
    /// `Some` while the `:` command line is open.
    pub command: Option<String>,
    pub dirty: bool,
    /// First visible line.
    pub scroll: usize,
    /// First visible column (chars) — horizontal scroll offset.
    pub hscroll: usize,
    pub status: Option<String>,
    /// Bumped on every buffer modification (drives LSP didChange sync).
    pub revision: u64,
    /// While true the viewport follows the cursor; wheel-scrolling detaches
    /// it (free scrolling), any key press re-attaches.
    pub follow_cursor: bool,
    /// A `g` prefix is pending (gg / ge / gh / gl).
    pending_g: bool,
    /// A space prefix is pending (space-k = hover).
    pending_space: bool,
    goal_col: Option<usize>,
    yank: String,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Set while an insert-mode burst has already pushed its undo point.
    insert_undo_open: bool,
    highlighter: Option<highlight::FileHighlighter>,
    highlights: Vec<HighlightSpan>,
    highlights_dirty: bool,
    /// Quantized scroll position the cached spans were extracted around.
    hl_base: Option<usize>,
    /// Spell-check comments/strings (toggled with `:spell`).
    pub spell_check: bool,
    /// Underline non-ASCII characters (toggled with `:unicode`) — surfaces
    /// accidental Unicode, confusables, and smart quotes in source.
    pub mark_unicode: bool,
}

impl Editor {
    pub fn open(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        Ok(Self {
            highlighter: highlight::config_for(&path).and_then(highlight::FileHighlighter::new),
            path,
            text: Rope::from_str(&text),
            mode: Mode::Normal,
            anchor: 0,
            head: 0,
            command: None,
            dirty: false,
            scroll: 0,
            hscroll: 0,
            status: None,
            revision: 0,
            follow_cursor: true,
            pending_g: false,
            pending_space: false,
            goal_col: None,
            yank: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            insert_undo_open: false,
            highlights: Vec::new(),
            highlights_dirty: true,
            hl_base: None,
            spell_check: crate::spell::available(),
            mark_unicode: true,
        })
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn save(&mut self) -> Result<()> {
        std::fs::write(&self.path, self.text.to_string())?;
        self.dirty = false;
        self.status = Some(format!(
            "wrote {} ({} lines)",
            self.file_name(),
            self.text.len_lines()
        ));
        Ok(())
    }

    /// Styled spans around the visible source, recomputed lazily after
    /// edits. Incremental twice over: the highlighter keeps the parse tree
    /// between edits (Tree::edit + reparse of the changed region), and the
    /// span extraction is windowed to the viewport plus a margin — the
    /// window is quantized so ordinary scrolling reuses the cached spans.
    pub fn highlights(&mut self) -> &[HighlightSpan] {
        // requery when scroll crosses a step; margin keeps the window far
        // wider than any terminal, so the visible lines are always covered
        const STEP: usize = 256;
        const MARGIN: usize = 512;
        let base = self.scroll / STEP;
        if self.highlights_dirty || self.hl_base != Some(base) {
            let lo = self.text.line_to_byte((base * STEP).saturating_sub(MARGIN));
            let hi_line = (base * STEP + STEP + MARGIN).min(self.text.len_lines());
            let hi = self.text.line_to_byte(hi_line);
            self.highlights = match &mut self.highlighter {
                Some(h) if self.highlights_dirty => {
                    h.highlight_window(self.text.to_string(), Some(lo..hi))
                }
                // buffer unchanged, window moved: query the existing tree
                Some(h) => h.window_only(lo..hi),
                None => Vec::new(),
            };
            self.hl_base = Some(base);
            self.highlights_dirty = false;
        }
        &self.highlights
    }

    /// Selection as an inclusive-exclusive char range covering the cursor.
    pub fn selection(&self) -> (usize, usize) {
        let (lo, hi) = (self.anchor.min(self.head), self.anchor.max(self.head));
        (lo, (hi + 1).min(self.text.len_chars().max(1)))
    }

    pub fn cursor_line_col(&self) -> (usize, usize) {
        let line = self.text.char_to_line(self.head.min(self.text.len_chars()));
        let col = self.head - self.text.line_to_char(line);
        (line, col)
    }

    /// Cursor position as LSP wants it: (line, UTF-16 column).
    pub fn cursor_lsp_position(&self) -> (usize, usize) {
        let (line, col) = self.cursor_line_col();
        let line_text = self.text.line(line).to_string();
        (line, crate::lsp::char_to_utf16_col(&line_text, col))
    }

    /// Keep the cursor line inside a viewport of `height` rows, with a small
    /// scroll-off margin.
    pub fn ensure_visible(&mut self, height: usize) {
        let (line, _) = self.cursor_line_col();
        let margin = 3.min(height.saturating_sub(1) / 2);
        if line < self.scroll + margin {
            self.scroll = line.saturating_sub(margin);
        }
        let bottom = self.scroll + height.saturating_sub(1).saturating_sub(margin);
        if line > bottom {
            self.scroll = line + margin + 1 - height.min(line + margin + 1);
        }
        let max_scroll = self.text.len_lines().saturating_sub(1);
        self.scroll = self.scroll.min(max_scroll);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.follow_cursor = false;
        let max_scroll = self.text.len_lines().saturating_sub(1);
        self.scroll = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            (self.scroll + delta as usize).min(max_scroll)
        };
    }

    /// Keep the cursor column inside a viewport of `width` cells, with a
    /// small scroll-off margin -- the horizontal twin of ensure_visible.
    pub fn ensure_visible_cols(&mut self, width: usize) {
        let (_, col) = self.cursor_line_col();
        let margin = 4.min(width.saturating_sub(1) / 2);
        if col < self.hscroll + margin {
            self.hscroll = col.saturating_sub(margin);
        }
        let right = self.hscroll + width.saturating_sub(1).saturating_sub(margin);
        if col > right {
            self.hscroll = (col + margin + 1).saturating_sub(width.min(col + margin + 1));
        }
    }

    /// Horizontal wheel scrolling; like scroll_by it detaches the viewport
    /// from the cursor until the next key press. Clamped left at 0 here,
    /// and right against the widest visible line at render time.
    pub fn hscroll_by(&mut self, delta: isize) {
        self.follow_cursor = false;
        self.hscroll = if delta < 0 {
            self.hscroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.hscroll + delta as usize
        };
    }

    /// Jump the cursor to (line, char column), collapsing the selection.
    pub fn jump_to(&mut self, line: usize, col: usize) {
        let mut line = line.min(self.text.len_lines().saturating_sub(1));
        // don't land on the phantom line after a trailing newline
        while line > 0 && self.line_content_len(line) == 0 && line + 1 == self.text.len_lines() {
            line -= 1;
        }
        let start = self.text.line_to_char(line);
        self.head = start + col.min(self.line_content_len(line).saturating_sub(1));
        self.anchor = self.head;
        self.goal_col = None;
        self.follow_cursor = true;
    }

    /// Jump the cursor to an absolute char index (for the jump-back stack).
    pub fn jump_to_char(&mut self, pos: usize) {
        self.head = pos.min(self.text.len_chars().saturating_sub(1));
        self.anchor = self.head;
        self.goal_col = None;
        self.follow_cursor = true;
    }

    /// Select the entire buffer (Ctrl+A).
    pub fn select_all(&mut self) {
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
        }
        self.anchor = 0;
        self.head = self.text.len_chars().saturating_sub(1);
        self.goal_col = None;
    }

    /// Smart-select: a double-click just inside a quote pair grabs
    /// the whole string contents between the quotes (same line only).
    /// Returns the inclusive char range of the contents, if any.
    fn quoted_range(&self, i: usize) -> Option<(usize, usize)> {
        let is_quote = |c: char| matches!(c, '"' | '\'' | '`');
        let n = self.text.len_chars();
        // cursor right after an opening quote: scan forward for the mate
        if i > 0 && is_quote(self.text.char(i - 1)) {
            let q = self.text.char(i - 1);
            let mut j = i;
            while j < n && self.text.char(j) != '\n' {
                if self.text.char(j) == q {
                    return (j > i).then_some((i, j - 1));
                }
                j += 1;
            }
        }
        // cursor on the closing quote: scan backward for the mate
        if i < n && is_quote(self.text.char(i)) {
            let q = self.text.char(i);
            let mut j = i;
            while j > 0 && self.text.char(j - 1) != '\n' {
                j -= 1;
                if self.text.char(j) == q {
                    return (j + 1 < i).then_some((j + 1, i - 1));
                }
            }
        }
        None
    }

    /// Select the word under the cursor (double-click): the run of
    /// word/punctuation/space characters around the cursor.
    pub fn select_word(&mut self) {
        let n = self.text.len_chars();
        if n == 0 {
            return;
        }
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
        }
        let i = self.head.min(n - 1);
        if let Some((start, end)) = self.quoted_range(i) {
            self.anchor = start;
            self.head = end;
            self.goal_col = None;
            return;
        }
        let class = class_of(self.text.char(i));
        let same = |c: char, class: &CharClass| {
            c != '\n'
                && matches!(
                    (class_of(c), class),
                    (CharClass::Word, CharClass::Word)
                        | (CharClass::Punct, CharClass::Punct)
                        | (CharClass::Space, CharClass::Space)
                )
        };
        let mut start = i;
        while start > 0 && same(self.text.char(start - 1), &class) {
            start -= 1;
        }
        let mut end = i;
        while end + 1 < n && same(self.text.char(end + 1), &class) {
            end += 1;
        }
        self.anchor = start;
        self.head = end;
        self.goal_col = None;
    }

    /// Place cursor from a viewport click at (visible_row, col).
    pub fn click(&mut self, row: usize, col: usize, extend: bool) {
        // sweeping a selection pulls insert mode back to normal so the
        // selection is visible and d/c/y can act on it
        if extend && self.mode == Mode::Insert {
            self.mode = Mode::Normal;
        }
        let line = (self.scroll + row).min(self.text.len_lines().saturating_sub(1));
        let start = self.text.line_to_char(line);
        let len = self.line_content_len(line);
        self.head = start + (self.hscroll + col).min(len.saturating_sub(1));
        if !extend {
            self.anchor = self.head;
        }
        self.goal_col = None;
    }

    // ----- key handling ---------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorEvent {
        self.status = None;
        self.follow_cursor = true;
        if self.command.is_some() {
            return self.handle_command_key(key);
        }
        // standard GUI editing shortcuts, active in every mode
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.clipboard_copy();
                    return EditorEvent::None;
                }
                KeyCode::Char('x') => {
                    self.clipboard_cut();
                    return EditorEvent::None;
                }
                KeyCode::Char('v') => {
                    self.clipboard_paste();
                    return EditorEvent::None;
                }
                // Ctrl+Z undo, Ctrl+Y or Ctrl+Shift+Z redo
                KeyCode::Char('z' | 'Z') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.redo();
                    } else {
                        self.undo();
                    }
                    return EditorEvent::None;
                }
                KeyCode::Char('y') => {
                    self.redo();
                    return EditorEvent::None;
                }
                _ => {}
            }
        }
        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Normal | Mode::Select => self.handle_normal_key(key),
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> EditorEvent {
        let Some(buf) = &mut self.command else {
            return EditorEvent::None;
        };
        match key.code {
            KeyCode::Esc => self.command = None,
            KeyCode::Backspace => {
                if buf.pop().is_none() {
                    self.command = None;
                }
            }
            KeyCode::Enter => {
                let cmd = buf.trim().to_string();
                self.command = None;
                return self.run_command(&cmd);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => buf.push(c),
            _ => {}
        }
        EditorEvent::None
    }

    fn run_command(&mut self, cmd: &str) -> EditorEvent {
        match cmd {
            "" => {}
            "w" | "write" => {
                return match self.save() {
                    Ok(()) => EditorEvent::Saved,
                    Err(e) => {
                        self.status = Some(format!("save failed: {e}"));
                        EditorEvent::None
                    }
                };
            }
            "q" | "quit" => {
                if self.dirty {
                    self.status = Some("unsaved changes (:q! to discard, :wq to save)".into());
                } else {
                    return EditorEvent::Close;
                }
            }
            "spell" => {
                self.spell_check = !self.spell_check;
                let state = if self.spell_check { "on" } else { "off" };
                self.status = Some(if crate::spell::available() {
                    format!("spell check {state}")
                } else {
                    "no dictionary found".into()
                });
            }
            "unicode" => {
                self.mark_unicode = !self.mark_unicode;
                let state = if self.mark_unicode { "on" } else { "off" };
                self.status = Some(format!("non-ASCII underline {state}"));
            }
            "q!" => return EditorEvent::Close,
            "wq" | "x" => {
                return match self.save() {
                    Ok(()) => EditorEvent::Close,
                    Err(e) => {
                        self.status = Some(format!("save failed: {e}"));
                        EditorEvent::None
                    }
                };
            }
            other => {
                if let Ok(line) = other.parse::<usize>() {
                    let line = line.saturating_sub(1).min(self.text.len_lines().saturating_sub(1));
                    self.head = self.text.line_to_char(line);
                    self.anchor = self.head;
                } else {
                    self.status = Some(format!("unknown command: {other}"));
                }
            }
        }
        EditorEvent::None
    }

    fn handle_insert_key(&mut self, key: KeyEvent) -> EditorEvent {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.insert_undo_open = false;
                // land the cursor on the char left of the insertion point,
                // standard modal behavior when leaving insert
                let (line, col) = self.insert_line_col();
                if col > 0 {
                    self.head -= 1;
                }
                let _ = line;
                self.clamp_cursor();
                self.anchor = self.head;
            }
            KeyCode::Enter => self.insert_text("\n"),
            KeyCode::Tab => self.insert_text("    "),
            KeyCode::Backspace => {
                if self.head > 0 {
                    self.begin_insert_edit();
                    self.text.remove(self.head - 1..self.head);
                    self.head -= 1;
                    self.anchor = self.head;
                    self.mark_edited();
                }
            }
            KeyCode::Delete => {
                if self.head < self.text.len_chars() {
                    self.begin_insert_edit();
                    self.text.remove(self.head..self.head + 1);
                    self.mark_edited();
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                // start a selection from insert mode
                self.mode = Mode::Normal;
                self.insert_undo_open = false;
                self.clamp_cursor();
                self.anchor = self.head;
                match key.code {
                    KeyCode::Left => self.move_horizontal(-1, true),
                    KeyCode::Right => self.move_horizontal(1, true),
                    KeyCode::Up => self.move_vertical(false, true),
                    _ => self.move_vertical(true, true),
                }
            }
            KeyCode::Left => self.head = self.head.saturating_sub(1),
            KeyCode::Right => self.head = (self.head + 1).min(self.text.len_chars()),
            KeyCode::Up | KeyCode::Down => {
                self.move_vertical(key.code == KeyCode::Down, true);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut buf = [0u8; 4];
                self.insert_text(c.encode_utf8(&mut buf));
            }
            _ => {}
        }
        EditorEvent::None
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> EditorEvent {
        // shift+arrows extend the selection, like every GUI editor
        let extend = self.mode == Mode::Select || key.modifiers.contains(KeyModifiers::SHIFT);
        if self.pending_space {
            self.pending_space = false;
            if key.code == KeyCode::Char('k') {
                return EditorEvent::Hover;
            }
            return EditorEvent::None;
        }
        if self.pending_g {
            self.pending_g = false;
            match key.code {
                KeyCode::Char('d') => return EditorEvent::GotoDefinition,
                KeyCode::Char('g') => self.goto_char(0, extend),
                KeyCode::Char('e') => {
                    self.goto_char(self.text.len_chars().saturating_sub(1), extend)
                }
                KeyCode::Char('h') => {
                    let (line, _) = self.cursor_line_col();
                    self.goto_char(self.text.line_to_char(line), extend);
                }
                KeyCode::Char('l') => {
                    let (line, _) = self.cursor_line_col();
                    let len = self.line_content_len(line);
                    self.goto_char(self.text.line_to_char(line) + len.saturating_sub(1), extend);
                }
                _ => {}
            }
            return EditorEvent::None;
        }
        match key.code {
            KeyCode::Esc => {
                // layered escape: leave select mode → collapse selection →
                // nothing left to cancel: let the app take focus back
                if self.mode == Mode::Normal && self.anchor == self.head {
                    return EditorEvent::FocusOut;
                }
                self.mode = Mode::Normal;
                self.anchor = self.head;
            }
            KeyCode::Char('v') => {
                self.mode = if self.mode == Mode::Select { Mode::Normal } else { Mode::Select };
            }
            KeyCode::Char(';') => self.anchor = self.head,
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char(' ') => self.pending_space = true,
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return EditorEvent::JumpBack;
            }
            // shift turns letters uppercase, unlike arrows which keep their
            // keycode — match both so shift+hjkl extends like shift+arrows
            KeyCode::Char('h' | 'H') | KeyCode::Left => self.move_horizontal(-1, extend),
            KeyCode::Char('l' | 'L') | KeyCode::Right => self.move_horizontal(1, extend),
            KeyCode::Char('k' | 'K') | KeyCode::Up => self.move_vertical(false, extend),
            KeyCode::Char('j' | 'J') | KeyCode::Down => self.move_vertical(true, extend),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_backward(),
            KeyCode::Char('e') => self.word_end(),
            KeyCode::Char('x') => self.extend_line(),
            KeyCode::Char('d') => self.delete_selection(false),
            KeyCode::Char('c') => self.delete_selection(true),
            KeyCode::Char('y') => self.yank_selection(),
            KeyCode::Char('p') => self.paste(true),
            KeyCode::Char('P') => self.paste(false),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('U') => self.redo(),
            KeyCode::Char('i') => self.enter_insert(self.selection().0),
            KeyCode::Char('a') => self.enter_insert(self.selection().1),
            KeyCode::Char('I') => {
                let (line, _) = self.cursor_line_col();
                self.enter_insert(self.first_nonblank(line));
            }
            KeyCode::Char('A') => {
                let (line, _) = self.cursor_line_col();
                let pos = self.text.line_to_char(line) + self.line_content_len(line);
                self.enter_insert(pos);
            }
            KeyCode::Char('o') => self.open_line(true),
            KeyCode::Char('O') => self.open_line(false),
            KeyCode::Char(':') => self.command = Some(String::new()),
            _ => {}
        }
        EditorEvent::None
    }

    // ----- movement -------------------------------------------------------

    fn goto_char(&mut self, pos: usize, extend: bool) {
        self.head = pos.min(self.text.len_chars().saturating_sub(1));
        if !extend {
            self.anchor = self.head;
        }
        self.goal_col = None;
    }

    fn move_horizontal(&mut self, dir: isize, extend: bool) {
        let (line, col) = self.cursor_line_col();
        if dir < 0 {
            if col > 0 {
                self.head -= 1;
            }
        } else {
            let len = self.line_content_len(line);
            if col + 1 < len.max(1) {
                self.head += 1;
            }
        }
        if !extend {
            self.anchor = self.head;
        }
        self.goal_col = None;
    }

    fn move_vertical(&mut self, down: bool, extend: bool) {
        let (line, col) = self.cursor_line_col();
        let goal = *self.goal_col.get_or_insert(col);
        let target = if down {
            if line + 1 >= self.text.len_lines() {
                return;
            }
            line + 1
        } else {
            if line == 0 {
                return;
            }
            line - 1
        };
        let len = if self.mode == Mode::Insert {
            self.line_content_len(target)
        } else {
            self.line_content_len(target).saturating_sub(1)
        };
        self.head = self.text.line_to_char(target) + goal.min(len);
        if !extend {
            self.anchor = self.head;
        }
    }

    fn word_forward(&mut self) {
        let n = self.text.len_chars();
        if n == 0 {
            return;
        }
        let start = self.head;
        let mut i = self.head;
        let class = class_of(self.text.char(i.min(n - 1)));
        // skip the rest of the current run, then whitespace
        while i < n && matches!((class_of(self.text.char(i)), &class), (CharClass::Word, CharClass::Word) | (CharClass::Punct, CharClass::Punct)) {
            i += 1;
        }
        while i < n && self.text.char(i).is_whitespace() {
            i += 1;
        }
        self.anchor = start;
        self.head = i.min(n.saturating_sub(1));
        self.goal_col = None;
    }

    fn word_backward(&mut self) {
        if self.head == 0 {
            return;
        }
        let start = self.head;
        let mut i = self.head;
        while i > 0 && self.text.char(i - 1).is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let class = class_of(self.text.char(i - 1));
            while i > 0
                && matches!(
                    (class_of(self.text.char(i - 1)), &class),
                    (CharClass::Word, CharClass::Word) | (CharClass::Punct, CharClass::Punct)
                )
            {
                i -= 1;
            }
        }
        self.anchor = start;
        self.head = i;
        self.goal_col = None;
    }

    fn word_end(&mut self) {
        let n = self.text.len_chars();
        if n == 0 {
            return;
        }
        let start = self.head;
        let mut i = (self.head + 1).min(n - 1);
        while i < n && self.text.char(i).is_whitespace() {
            i += 1;
        }
        if i < n {
            let class = class_of(self.text.char(i));
            while i + 1 < n
                && matches!(
                    (class_of(self.text.char(i + 1)), &class),
                    (CharClass::Word, CharClass::Word) | (CharClass::Punct, CharClass::Punct)
                )
            {
                i += 1;
            }
        }
        self.anchor = start;
        self.head = i.min(n - 1);
        self.goal_col = None;
    }

    /// `x`: select the whole current line; pressing again extends the
    /// selection one line down.
    fn extend_line(&mut self) {
        let (lo, hi) = (self.anchor.min(self.head), self.anchor.max(self.head));
        let first = self.text.char_to_line(lo);
        let last = self.text.char_to_line(hi.min(self.text.len_chars().saturating_sub(1)));
        let line_start = self.text.line_to_char(first);
        let line_end = self.line_end_inclusive(last);
        if self.anchor == line_start && self.head == line_end && last + 1 < self.text.len_lines() {
            self.head = self.line_end_inclusive(last + 1);
        } else {
            self.anchor = line_start;
            self.head = line_end;
        }
        self.goal_col = None;
    }

    // ----- editing --------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            anchor: self.anchor,
            head: self.head,
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        self.redo_stack.clear();
    }

    /// Insert-mode edits share one undo point per insert session.
    fn begin_insert_edit(&mut self) {
        if !self.insert_undo_open {
            self.push_undo();
            self.insert_undo_open = true;
        }
    }

    fn mark_edited(&mut self) {
        self.dirty = true;
        self.highlights_dirty = true;
        self.revision += 1;
    }

    fn insert_text(&mut self, s: &str) {
        self.begin_insert_edit();
        self.text.insert(self.head, s);
        self.head += s.chars().count();
        self.anchor = self.head;
        self.goal_col = None;
        self.mark_edited();
    }

    fn delete_selection(&mut self, then_insert: bool) {
        if self.text.len_chars() == 0 {
            if then_insert {
                self.enter_insert(0);
            }
            return;
        }
        self.push_undo();
        let (lo, hi) = self.selection();
        let hi = hi.min(self.text.len_chars());
        self.yank = self.text.slice(lo..hi).to_string();
        self.text.remove(lo..hi);
        self.head = lo.min(self.text.len_chars().saturating_sub(1));
        self.anchor = self.head;
        self.mark_edited();
        if then_insert {
            self.enter_insert(lo);
        } else {
            self.clamp_cursor();
        }
    }

    fn yank_selection(&mut self) {
        let (lo, hi) = self.selection();
        let hi = hi.min(self.text.len_chars());
        self.yank = self.text.slice(lo..hi).to_string();
        self.status = Some(format!("yanked {} chars", hi - lo));
    }

    /// A real, user-made selection (as opposed to the implicit 1-char
    /// cursor selection of Normal mode).
    fn has_selection(&self) -> bool {
        self.anchor != self.head
    }

    /// The char range of the current line, including its trailing newline.
    fn line_range(&self) -> (usize, usize) {
        let (line, _) = self.cursor_line_col();
        let start = self.text.line_to_char(line);
        let end = if line + 1 < self.text.len_lines() {
            self.text.line_to_char(line + 1)
        } else {
            self.text.len_chars()
        };
        (start, end)
    }

    /// Ctrl+C — copy the selection to the system clipboard. A no-op when
    /// nothing is selected (the "performable, else do nothing" convention),
    /// so it never silently grabs a line you didn't mean to copy.
    fn clipboard_copy(&mut self) {
        if !self.has_selection() {
            return;
        }
        let (lo, hi) = self.selection();
        let hi = hi.min(self.text.len_chars());
        if hi <= lo {
            return;
        }
        crate::clipboard::set(&self.text.slice(lo..hi).to_string());
        self.status = Some("copied".into());
    }

    /// Ctrl+X — cut the selection (or the whole line) to the clipboard.
    fn clipboard_cut(&mut self) {
        let (lo, hi) = if self.has_selection() {
            let (lo, hi) = self.selection();
            (lo, hi.min(self.text.len_chars()))
        } else {
            self.line_range()
        };
        if hi <= lo {
            return;
        }
        self.push_undo();
        crate::clipboard::set(&self.text.slice(lo..hi).to_string());
        self.text.remove(lo..hi);
        self.head = lo.min(self.text.len_chars().saturating_sub(1));
        self.anchor = self.head;
        self.goal_col = None;
        self.mark_edited();
        if self.mode != Mode::Insert {
            self.clamp_cursor();
        }
        self.status = Some("cut".into());
    }

    /// Ctrl+V — paste the clipboard at the cursor, replacing any selection.
    fn clipboard_paste(&mut self) {
        match crate::clipboard::get().filter(|t| !t.is_empty()) {
            Some(text) => self.paste_str(&text),
            None => self.status = Some("clipboard empty".into()),
        }
    }

    /// Insert `text` at the cursor, replacing any selection. Shared by
    /// Ctrl+V and the terminal's bracketed paste (Cmd+V on macOS).
    pub fn paste_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.push_undo();
        let at = if self.has_selection() {
            let (lo, hi) = self.selection();
            let hi = hi.min(self.text.len_chars());
            self.text.remove(lo..hi);
            lo
        } else {
            self.head.min(self.text.len_chars())
        };
        self.text.insert(at, text);
        self.head = at + text.chars().count();
        self.anchor = self.head;
        self.goal_col = None;
        self.mark_edited();
        if self.mode != Mode::Insert {
            self.clamp_cursor();
        }
        self.status = Some("pasted".into());
    }

    fn paste(&mut self, after: bool) {
        if self.yank.is_empty() {
            return;
        }
        self.push_undo();
        let linewise = self.yank.ends_with('\n');
        let pos = if linewise {
            let (line, _) = self.cursor_line_col();
            if after {
                let next = (line + 1).min(self.text.len_lines());
                if next >= self.text.len_lines() {
                    self.text.len_chars()
                } else {
                    self.text.line_to_char(next)
                }
            } else {
                self.text.line_to_char(line)
            }
        } else if after {
            self.selection().1
        } else {
            self.selection().0
        };
        let yank = self.yank.clone();
        self.text.insert(pos, &yank);
        self.anchor = pos;
        self.head = pos + yank.chars().count().saturating_sub(1);
        self.mark_edited();
    }

    fn undo(&mut self) {
        if let Some(snap) = self.undo_stack.pop() {
            self.redo_stack.push(self.snapshot());
            self.restore(snap);
            self.status = Some("undo".into());
        } else {
            self.status = Some("already at oldest change".into());
        }
    }

    fn redo(&mut self) {
        if let Some(snap) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot());
            self.restore(snap);
            self.status = Some("redo".into());
        } else {
            self.status = Some("already at newest change".into());
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        self.text = snap.text;
        self.anchor = snap.anchor;
        self.head = snap.head;
        self.mark_edited();
        self.clamp_cursor();
    }

    fn enter_insert(&mut self, pos: usize) {
        self.mode = Mode::Insert;
        self.head = pos.min(self.text.len_chars());
        self.anchor = self.head;
        self.insert_undo_open = false;
        self.goal_col = None;
    }

    fn open_line(&mut self, below: bool) {
        self.push_undo();
        self.insert_undo_open = true;
        let (line, _) = self.cursor_line_col();
        let indent: String = self
            .text
            .line(line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let pos = if below {
            self.text.line_to_char(line) + self.line_content_len(line)
        } else {
            self.text.line_to_char(line)
        };
        let inserted = if below {
            format!("\n{indent}")
        } else {
            format!("{indent}\n")
        };
        self.text.insert(pos, &inserted);
        self.mode = Mode::Insert;
        self.head = if below {
            pos + inserted.chars().count()
        } else {
            pos + indent.chars().count()
        };
        self.anchor = self.head;
        self.mark_edited();
    }

    // ----- helpers ---------------------------------------------------------

    /// Length of a line's content excluding the trailing newline.
    fn line_content_len(&self, line: usize) -> usize {
        let l = self.text.line(line);
        let mut len = l.len_chars();
        if len > 0 && l.char(len - 1) == '\n' {
            len -= 1;
        }
        len
    }

    /// Char index of the line's last char (the newline, or last content
    /// char on the final line) — `x` selects through the newline.
    fn line_end_inclusive(&self, line: usize) -> usize {
        let start = self.text.line_to_char(line);
        let len = self.text.line(line).len_chars();
        (start + len.saturating_sub(1)).max(start)
    }

    fn first_nonblank(&self, line: usize) -> usize {
        let start = self.text.line_to_char(line);
        let blank = self
            .text
            .line(line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count();
        start + blank
    }

    fn insert_line_col(&self) -> (usize, usize) {
        let line = self.text.char_to_line(self.head.min(self.text.len_chars()));
        (line, self.head - self.text.line_to_char(line))
    }

    fn clamp_cursor(&mut self) {
        let n = self.text.len_chars();
        if n == 0 {
            self.head = 0;
            self.anchor = 0;
            return;
        }
        self.head = self.head.min(n - 1);
        self.anchor = self.anchor.min(n - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn editor_with(content: &str) -> (TempDir, Editor) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, content).unwrap();
        (dir, Editor::open(&path).unwrap())
    }

    fn press(ed: &mut Editor, code: KeyCode) -> EditorEvent {
        ed.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_str(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            press(ed, KeyCode::Char(c));
        }
    }

    fn ctrl(ed: &mut Editor, code: KeyCode) -> EditorEvent {
        ed.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL))
    }

    // one test: the mock clipboard is a shared static, so keep its uses
    // sequential in a single test to avoid cross-test races
    #[test]
    fn ctrl_c_v_x_use_the_system_clipboard() {
        // Ctrl+C with no selection is a no-op (leaves the clipboard alone)
        crate::clipboard::set("sentinel");
        let (_d0, mut ed0) = editor_with("abc\n");
        ctrl(&mut ed0, KeyCode::Char('c'));
        assert_eq!(crate::clipboard::get().as_deref(), Some("sentinel"), "no-op, nothing selected");

        // Ctrl+C copies a selection
        let (_d, mut ed) = editor_with("hello world\n");
        // inclusive selection: 4 shift-rights covers h,e,l,l,o
        for _ in 0..4 {
            ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        }
        ctrl(&mut ed, KeyCode::Char('c'));
        assert_eq!(crate::clipboard::get().as_deref(), Some("hello"));

        // Ctrl+V pastes at the cursor (no selection → plain insert at head 0)
        let (_d2, mut ed2) = editor_with("XY\n");
        ctrl(&mut ed2, KeyCode::Char('v'));
        assert_eq!(ed2.text.to_string(), "helloXY\n");

        // Ctrl+X with no selection cuts the whole line
        let (_d3, mut ed3) = editor_with("first\nsecond\n");
        press(&mut ed3, KeyCode::Char('j')); // to line 2
        ctrl(&mut ed3, KeyCode::Char('x'));
        assert_eq!(crate::clipboard::get().as_deref(), Some("second\n"));
        assert_eq!(ed3.text.to_string(), "first\n");

        // Ctrl+V replaces an active selection
        let (_d4, mut ed4) = editor_with("aXYb\n");
        press(&mut ed4, KeyCode::Char('l')); // cursor on 'X'
        ed4.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)); // select "XY"
        ctrl(&mut ed4, KeyCode::Char('v')); // paste "second\n" over "XY"
        assert_eq!(ed4.text.to_string(), "asecond\nb\n");
    }

    #[test]
    fn ctrl_z_and_ctrl_y_undo_and_redo() {
        let (_d, mut ed) = editor_with("abc\n");
        press(&mut ed, KeyCode::Char('i'));
        type_str(&mut ed, "XY");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.text.to_string(), "XYabc\n");
        ctrl(&mut ed, KeyCode::Char('z')); // undo
        assert_eq!(ed.text.to_string(), "abc\n");
        ctrl(&mut ed, KeyCode::Char('y')); // redo
        assert_eq!(ed.text.to_string(), "XYabc\n");
        // Ctrl+Shift+Z also redoes
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(ed.text.to_string(), "abc\n");
        ed.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        assert_eq!(ed.text.to_string(), "XYabc\n");
    }

    #[test]
    fn hjkl_movement_and_line_clamping() {
        let (_d, mut ed) = editor_with("abc\nde\nfghij\n");
        press(&mut ed, KeyCode::Char('l'));
        assert_eq!(ed.head, 1);
        press(&mut ed, KeyCode::Char('j')); // to "de", col clamped later
        assert_eq!(ed.cursor_line_col(), (1, 1));
        press(&mut ed, KeyCode::Char('j')); // to "fghij", goal col preserved
        assert_eq!(ed.cursor_line_col(), (2, 1));
        press(&mut ed, KeyCode::Char('k'));
        press(&mut ed, KeyCode::Char('k'));
        assert_eq!(ed.cursor_line_col(), (0, 1));
        press(&mut ed, KeyCode::Char('h'));
        assert_eq!(ed.head, 0);
        press(&mut ed, KeyCode::Char('h')); // clamped at line start
        assert_eq!(ed.head, 0);
    }

    #[test]
    fn goal_column_persists_across_short_lines() {
        let (_d, mut ed) = editor_with("abcde\nx\nabcde\n");
        for _ in 0..4 {
            press(&mut ed, KeyCode::Char('l'));
        }
        assert_eq!(ed.cursor_line_col(), (0, 4));
        press(&mut ed, KeyCode::Char('j'));
        assert_eq!(ed.cursor_line_col(), (1, 0));
        press(&mut ed, KeyCode::Char('j'));
        assert_eq!(ed.cursor_line_col(), (2, 4));
    }

    #[test]
    fn insert_mode_types_and_escapes() {
        let (_d, mut ed) = editor_with("world\n");
        press(&mut ed, KeyCode::Char('i'));
        assert_eq!(ed.mode, Mode::Insert);
        type_str(&mut ed, "hello ");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.text.to_string(), "hello world\n");
        assert!(ed.dirty);
    }

    #[test]
    fn a_appends_after_selection() {
        let (_d, mut ed) = editor_with("ab\n");
        press(&mut ed, KeyCode::Char('a'));
        type_str(&mut ed, "X");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.text.to_string(), "aXb\n");
    }

    #[test]
    fn open_line_below_keeps_indent() {
        let (_d, mut ed) = editor_with("    indented\n");
        press(&mut ed, KeyCode::Char('o'));
        assert_eq!(ed.mode, Mode::Insert);
        type_str(&mut ed, "x");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.text.to_string(), "    indented\n    x\n");
    }

    #[test]
    fn word_motions_select_span() {
        let (_d, mut ed) = editor_with("foo bar_baz qux\n");
        press(&mut ed, KeyCode::Char('w'));
        assert_eq!(ed.anchor, 0);
        assert_eq!(ed.head, 4); // on 'b' of bar_baz
        press(&mut ed, KeyCode::Char('e'));
        assert_eq!(ed.head, 10); // end of bar_baz
        press(&mut ed, KeyCode::Char('b'));
        assert_eq!(ed.head, 4);
    }

    #[test]
    fn x_selects_line_and_extends() {
        let (_d, mut ed) = editor_with("one\ntwo\nthree\n");
        press(&mut ed, KeyCode::Char('x'));
        let (lo, hi) = ed.selection();
        assert_eq!((lo, hi), (0, 4)); // "one\n"
        press(&mut ed, KeyCode::Char('x'));
        let (lo, hi) = ed.selection();
        assert_eq!((lo, hi), (0, 8)); // "one\ntwo\n"
    }

    #[test]
    fn delete_yanks_and_paste_restores() {
        let (_d, mut ed) = editor_with("one\ntwo\nthree\n");
        press(&mut ed, KeyCode::Char('x'));
        press(&mut ed, KeyCode::Char('d'));
        assert_eq!(ed.text.to_string(), "two\nthree\n");
        press(&mut ed, KeyCode::Char('p')); // paste line below cursor line
        assert_eq!(ed.text.to_string(), "two\none\nthree\n");
        press(&mut ed, KeyCode::Char('u'));
        assert_eq!(ed.text.to_string(), "two\nthree\n");
        press(&mut ed, KeyCode::Char('u'));
        assert_eq!(ed.text.to_string(), "one\ntwo\nthree\n");
        press(&mut ed, KeyCode::Char('U'));
        assert_eq!(ed.text.to_string(), "two\nthree\n");
    }

    #[test]
    fn change_deletes_and_enters_insert() {
        let (_d, mut ed) = editor_with("abc def\n");
        press(&mut ed, KeyCode::Char('w'));
        press(&mut ed, KeyCode::Char('c'));
        assert_eq!(ed.mode, Mode::Insert);
        type_str(&mut ed, "xyz ");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.text.to_string(), "xyz ef\n");
    }

    #[test]
    fn select_mode_extends_with_movement() {
        let (_d, mut ed) = editor_with("abcdef\n");
        press(&mut ed, KeyCode::Char('v'));
        assert_eq!(ed.mode, Mode::Select);
        press(&mut ed, KeyCode::Char('l'));
        press(&mut ed, KeyCode::Char('l'));
        let (lo, hi) = ed.selection();
        assert_eq!((lo, hi), (0, 3));
        press(&mut ed, KeyCode::Char('d'));
        assert_eq!(ed.text.to_string(), "def\n");
        assert_eq!(ed.mode, Mode::Select); // mode persists; Esc exits
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.mode, Mode::Normal);
    }

    #[test]
    fn escape_layers_then_focuses_out() {
        let (_d, mut ed) = editor_with("abcdef\n");
        // insert → normal
        press(&mut ed, KeyCode::Char('i'));
        assert_eq!(press(&mut ed, KeyCode::Esc), EditorEvent::None);
        assert_eq!(ed.mode, Mode::Normal);
        // selection → collapse
        press(&mut ed, KeyCode::Char('v'));
        press(&mut ed, KeyCode::Char('l'));
        assert_eq!(press(&mut ed, KeyCode::Esc), EditorEvent::None);
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.anchor, ed.head);
        // nothing to cancel → focus out
        assert_eq!(press(&mut ed, KeyCode::Esc), EditorEvent::FocusOut);
    }

    #[test]
    fn gd_and_ctrl_o_emit_events() {
        let (_d, mut ed) = editor_with("fn main() {}\n");
        press(&mut ed, KeyCode::Char('g'));
        assert_eq!(press(&mut ed, KeyCode::Char('d')), EditorEvent::GotoDefinition);
        assert_eq!(
            ed.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            EditorEvent::JumpBack
        );
    }

    #[test]
    fn jump_to_clamps_and_collapses() {
        let (_d, mut ed) = editor_with("short\nlonger line\n");
        ed.jump_to(1, 3);
        assert_eq!(ed.cursor_line_col(), (1, 3));
        assert_eq!(ed.anchor, ed.head);
        ed.jump_to(99, 99); // clamped
        assert_eq!(ed.cursor_line_col().0, 1);
        ed.jump_to_char(2);
        assert_eq!(ed.head, 2);
    }

    #[test]
    fn gg_ge_gh_gl_motions() {
        let (_d, mut ed) = editor_with("first\nsecond\nlast\n");
        press(&mut ed, KeyCode::Char('g'));
        press(&mut ed, KeyCode::Char('e'));
        assert_eq!(ed.cursor_line_col().0, 2);
        press(&mut ed, KeyCode::Char('g'));
        press(&mut ed, KeyCode::Char('g'));
        assert_eq!(ed.head, 0);
        press(&mut ed, KeyCode::Char('j'));
        press(&mut ed, KeyCode::Char('g'));
        press(&mut ed, KeyCode::Char('l'));
        assert_eq!(ed.cursor_line_col(), (1, 5)); // on 'd' of "second"
        press(&mut ed, KeyCode::Char('g'));
        press(&mut ed, KeyCode::Char('h'));
        assert_eq!(ed.cursor_line_col(), (1, 0));
    }

    #[test]
    fn command_write_and_quit() {
        let (dir, mut ed) = editor_with("hello\n");
        press(&mut ed, KeyCode::Char('a'));
        type_str(&mut ed, "!");
        press(&mut ed, KeyCode::Esc);
        press(&mut ed, KeyCode::Char(':'));
        assert!(ed.command.is_some());
        type_str(&mut ed, "w");
        press(&mut ed, KeyCode::Enter);
        assert!(!ed.dirty);
        let content = std::fs::read_to_string(dir.path().join("test.rs")).unwrap();
        assert_eq!(content, "h!ello\n");
        press(&mut ed, KeyCode::Char(':'));
        type_str(&mut ed, "q");
        assert_eq!(press(&mut ed, KeyCode::Enter), EditorEvent::Close);
    }

    #[test]
    fn quit_refuses_unsaved_then_force_quits() {
        let (_d, mut ed) = editor_with("hello\n");
        press(&mut ed, KeyCode::Char('a'));
        type_str(&mut ed, "x");
        press(&mut ed, KeyCode::Esc);
        press(&mut ed, KeyCode::Char(':'));
        type_str(&mut ed, "q");
        assert_eq!(press(&mut ed, KeyCode::Enter), EditorEvent::None);
        assert!(ed.status.as_deref().unwrap_or("").contains("unsaved"));
        press(&mut ed, KeyCode::Char(':'));
        type_str(&mut ed, "q!");
        assert_eq!(press(&mut ed, KeyCode::Enter), EditorEvent::Close);
    }

    #[test]
    fn goto_line_number_command() {
        let (_d, mut ed) = editor_with("a\nb\nc\nd\n");
        press(&mut ed, KeyCode::Char(':'));
        type_str(&mut ed, "3");
        press(&mut ed, KeyCode::Enter);
        assert_eq!(ed.cursor_line_col().0, 2);
    }

    #[test]
    fn insert_burst_is_one_undo() {
        let (_d, mut ed) = editor_with("x\n");
        press(&mut ed, KeyCode::Char('i'));
        type_str(&mut ed, "abc");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.text.to_string(), "abcx\n");
        press(&mut ed, KeyCode::Char('u'));
        assert_eq!(ed.text.to_string(), "x\n");
    }

    #[test]
    fn click_places_cursor_and_scroll_clamps() {
        let (_d, mut ed) = editor_with("aaaa\nbb\ncccc\n");
        ed.click(2, 3, false);
        assert_eq!(ed.cursor_line_col(), (2, 3));
        ed.click(1, 10, false); // col clamped to line content
        assert_eq!(ed.cursor_line_col(), (1, 1));
        ed.scroll_by(100);
        assert_eq!(ed.scroll, 3); // 3 lines + ropey's phantom line after trailing \n
        ed.scroll_by(-100);
        assert_eq!(ed.scroll, 0);
    }

    #[test]
    fn shift_arrows_select_in_normal_mode() {
        let (_d, mut ed) = editor_with("abcdef\nsecond\n");
        ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(ed.selection(), (0, 3));
        // shift+down extends across lines
        ed.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        let (lo, hi) = ed.selection();
        assert_eq!(lo, 0);
        assert!(hi > 7, "reaches into line 2: {hi}");
        // plain arrow collapses again
        ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let (lo, hi) = ed.selection();
        assert_eq!(hi - lo, 1);
    }

    #[test]
    fn shift_hjkl_selects_like_shift_arrows() {
        let (_d, mut ed) = editor_with("abcdef\nsecond\n");
        // crossterm delivers shift+l as uppercase 'L' with SHIFT set
        ed.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        ed.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert_eq!(ed.selection(), (0, 3));
        ed.handle_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
        let (lo, hi) = ed.selection();
        assert_eq!(lo, 0);
        assert!(hi > 7, "reaches into line 2: {hi}");
        ed.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
        ed.handle_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert_eq!(ed.selection(), (0, 2));
        // plain motion collapses again
        press(&mut ed, KeyCode::Char('l'));
        let (lo, hi) = ed.selection();
        assert_eq!(hi - lo, 1);
    }

    #[test]
    fn shift_arrows_select_from_insert_mode() {
        let (_d, mut ed) = editor_with("abcdef\n");
        press(&mut ed, KeyCode::Char('i'));
        ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(ed.mode, Mode::Normal);
        assert_eq!(ed.selection(), (0, 2));
        ed.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(ed.selection(), (0, 3));
        press(&mut ed, KeyCode::Char('d'));
        assert_eq!(ed.text.to_string(), "def\n");
    }

    #[test]
    fn select_word_grabs_identifier_under_cursor() {
        let (_d, mut ed) = editor_with("let foo_bar = baz;\n");
        ed.click(0, 6, false); // inside foo_bar
        ed.select_word();
        let (lo, hi) = ed.selection();
        assert_eq!(ed.text.slice(lo..hi).to_string(), "foo_bar");
        // on punctuation: selects the punct run
        ed.click(0, 12, false); // '='
        ed.select_word();
        let (lo, hi) = ed.selection();
        assert_eq!(ed.text.slice(lo..hi).to_string(), "=");
    }

    #[test]
    fn double_click_inside_quotes_selects_whole_string() {
        let (_d, mut ed) = editor_with("let s = \"tes t\";\n");
        // just after the opening quote (on the 't' of "tes")
        ed.click(0, 9, false);
        ed.select_word();
        let (lo, hi) = ed.selection();
        assert_eq!(ed.text.slice(lo..hi).to_string(), "tes t");
        // on the closing quote
        ed.click(0, 14, false);
        ed.select_word();
        let (lo, hi) = ed.selection();
        assert_eq!(ed.text.slice(lo..hi).to_string(), "tes t");
        // in the middle of a word inside the string: just that word
        ed.click(0, 10, false);
        ed.select_word();
        let (lo, hi) = ed.selection();
        assert_eq!(ed.text.slice(lo..hi).to_string(), "tes");
        // single quotes and backticks too
        let (_d2, mut ed2) = editor_with("x = 'a b'\n");
        ed2.click(0, 5, false);
        ed2.select_word();
        let (lo, hi) = ed2.selection();
        assert_eq!(ed2.text.slice(lo..hi).to_string(), "a b");
    }

    #[test]
    fn empty_or_unclosed_quotes_fall_back_to_word_select() {
        // empty string: nothing between the quotes, select the punct run
        let (_d, mut ed) = editor_with("s = \"\";\n");
        ed.click(0, 5, false);
        ed.select_word();
        let (lo, hi) = ed.selection();
        assert_eq!(ed.text.slice(lo..hi).to_string(), "\"\";");
        // unclosed quote: no mate on the line, plain word select
        let (_d2, mut ed2) = editor_with("say \"oops\n");
        ed2.click(0, 5, false);
        ed2.select_word();
        let (lo, hi) = ed2.selection();
        assert_eq!(ed2.text.slice(lo..hi).to_string(), "oops");
    }

    #[test]
    fn select_all_covers_buffer_and_leaves_insert() {
        let (_d, mut ed) = editor_with("one\ntwo\nthree\n");
        press(&mut ed, KeyCode::Char('i'));
        ed.select_all();
        assert_eq!(ed.mode, Mode::Normal);
        let (lo, hi) = ed.selection();
        assert_eq!(lo, 0);
        assert_eq!(hi, ed.text.len_chars());
        press(&mut ed, KeyCode::Char('d'));
        assert_eq!(ed.text.to_string(), "");
    }

    #[test]
    fn drag_click_leaves_insert_mode() {
        let (_d, mut ed) = editor_with("hello\n");
        press(&mut ed, KeyCode::Char('i'));
        assert_eq!(ed.mode, Mode::Insert);
        ed.click(0, 0, false);
        assert_eq!(ed.mode, Mode::Insert, "plain click keeps mode");
        ed.click(0, 3, true);
        assert_eq!(ed.mode, Mode::Normal, "drag returns to normal");
        assert_eq!(ed.selection(), (0, 4));
    }

    #[test]
    fn wheel_scroll_detaches_viewport_keys_reattach() {
        let content: String = (0..100).map(|i| format!("line{i}\n")).collect();
        let (_d, mut ed) = editor_with(&content);
        // wheel away from the cursor: viewport must stay put
        ed.scroll_by(50);
        assert!(!ed.follow_cursor);
        assert_eq!(ed.scroll, 50);
        // a key press re-attaches and ensure_visible may snap back
        press(&mut ed, KeyCode::Char('j'));
        assert!(ed.follow_cursor);
    }

    #[test]
    fn ensure_visible_scrolls_viewport() {
        let content: String = (0..100).map(|i| format!("line{i}\n")).collect();
        let (_d, mut ed) = editor_with(&content);
        press(&mut ed, KeyCode::Char(':'));
        type_str(&mut ed, "80");
        press(&mut ed, KeyCode::Enter);
        ed.ensure_visible(20);
        let (line, _) = ed.cursor_line_col();
        assert!(line >= ed.scroll && line < ed.scroll + 20, "line {line} scroll {}", ed.scroll);
    }

    #[test]
    fn horizontal_follow_keeps_cursor_in_view() {
        let long = format!("{}\n", "abcdefghij".repeat(20)); // 200-char line
        let (_d, mut ed) = editor_with(&long);
        // gl = end of line; a 40-col viewport must scroll right to show it
        press(&mut ed, KeyCode::Char('g'));
        press(&mut ed, KeyCode::Char('l'));
        ed.ensure_visible_cols(40);
        let (_, col) = ed.cursor_line_col();
        assert!(col >= ed.hscroll && col < ed.hscroll + 40, "col {col} hscroll {}", ed.hscroll);
        // gh = line start; the viewport follows back to column 0
        press(&mut ed, KeyCode::Char('g'));
        press(&mut ed, KeyCode::Char('h'));
        ed.ensure_visible_cols(40);
        assert_eq!(ed.hscroll, 0);
    }

    #[test]
    fn horizontal_wheel_pans_and_detaches() {
        let long = format!("{}\n", "x".repeat(120));
        let (_d, mut ed) = editor_with(&long);
        ed.hscroll_by(5);
        assert_eq!(ed.hscroll, 5);
        assert!(!ed.follow_cursor, "wheel panning detaches the viewport");
        ed.hscroll_by(-10);
        assert_eq!(ed.hscroll, 0, "clamped at the left edge");
        // clicking maps through the horizontal offset
        ed.hscroll = 50;
        ed.click(0, 3, false);
        let (_, col) = ed.cursor_line_col();
        assert_eq!(col, 53);
    }

    #[test]
    fn empty_file_is_safe() {
        let (_d, mut ed) = editor_with("");
        press(&mut ed, KeyCode::Char('l'));
        press(&mut ed, KeyCode::Char('x'));
        press(&mut ed, KeyCode::Char('d'));
        press(&mut ed, KeyCode::Char('w'));
        press(&mut ed, KeyCode::Char('i'));
        type_str(&mut ed, "new");
        press(&mut ed, KeyCode::Esc);
        assert_eq!(ed.text.to_string(), "new");
    }
}
