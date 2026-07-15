//! A single-line text input with a cursor and selection — the editing model
//! for small fields like the agent composer, without the full modal editor.
//! Char-indexed so multi-byte text behaves; selection anchors the way the
//! editor's does (Shift extends, a plain move collapses).

#[derive(Default, Clone)]
pub struct TextInput {
    chars: Vec<char>,
    /// Cursor position, 0..=len.
    cursor: usize,
    /// Selection anchor; the selection is anchor..cursor (either order).
    anchor: Option<usize>,
}

impl TextInput {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    /// Replace the whole buffer and place the cursor at `cursor` (char index,
    /// clamped). Used when the @-mention picker rewrites a token.
    pub fn set_text(&mut self, text: &str, cursor: usize) {
        self.chars = text.chars().collect();
        self.cursor = cursor.min(self.chars.len());
        self.anchor = None;
    }

    /// The selected range as ordered char indices, or None when empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.map(|a| (a.min(self.cursor), a.max(self.cursor))).filter(|(s, e)| s != e)
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.chars[s..e].iter().collect())
    }

    /// Delete the selection if any; returns whether it deleted something.
    fn delete_selection(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            self.chars.drain(s..e);
            self.cursor = s;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.delete_selection();
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        for c in s.chars() {
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    /// Ctrl+Backspace: delete the word before the cursor.
    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.prev_word();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Move the cursor to `pos`, extending the selection when `extend`.
    fn move_to(&mut self, pos: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos.min(self.chars.len());
        if self.anchor == Some(self.cursor) {
            self.anchor = None;
        }
    }

    pub fn left(&mut self, extend: bool) {
        // a plain Left with a selection collapses to its left edge
        if !extend && let Some((s, _)) = self.selection() {
            self.cursor = s;
            self.anchor = None;
            return;
        }
        self.move_to(self.cursor.saturating_sub(1), extend);
    }

    pub fn right(&mut self, extend: bool) {
        if !extend && let Some((_, e)) = self.selection() {
            self.cursor = e;
            self.anchor = None;
            return;
        }
        self.move_to(self.cursor + 1, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_to(self.chars.len(), extend);
    }

    pub fn word_left(&mut self, extend: bool) {
        self.move_to(self.prev_word(), extend);
    }

    pub fn word_right(&mut self, extend: bool) {
        self.move_to(self.next_word(), extend);
    }

    pub fn select_all(&mut self) {
        if self.chars.is_empty() {
            return;
        }
        self.anchor = Some(0);
        self.cursor = self.chars.len();
    }

    /// Start of the word before the cursor (skip whitespace, then word).
    fn prev_word(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    /// Start of the word after the cursor (skip word, then whitespace).
    fn next_word(&self) -> usize {
        let n = self.chars.len();
        let mut i = self.cursor;
        while i < n && !self.chars[i].is_whitespace() {
            i += 1;
        }
        while i < n && self.chars[i].is_whitespace() {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> TextInput {
        let mut t = TextInput::default();
        t.insert_str(s);
        t
    }

    #[test]
    fn typing_and_backspace() {
        let mut t = typed("hello");
        assert_eq!(t.text(), "hello");
        assert_eq!(t.cursor(), 5);
        t.backspace();
        assert_eq!(t.text(), "hell");
        t.home(false);
        t.delete();
        assert_eq!(t.text(), "ell");
    }

    #[test]
    fn cursor_movement_and_word_nav() {
        let mut t = typed("foo bar baz");
        t.home(false);
        assert_eq!(t.cursor(), 0);
        t.word_right(false);
        assert_eq!(t.cursor(), 4); // start of "bar"
        t.word_right(false);
        assert_eq!(t.cursor(), 8); // start of "baz"
        t.word_left(false);
        assert_eq!(t.cursor(), 4);
        t.end(false);
        assert_eq!(t.cursor(), 11);
    }

    #[test]
    fn selection_replace_and_copy() {
        let mut t = typed("hello world");
        t.home(false);
        t.word_right(true); // select "hello "
        assert_eq!(t.selected_text().as_deref(), Some("hello "));
        t.insert_str("hi "); // typing replaces the selection
        assert_eq!(t.text(), "hi world");
        assert!(t.selection().is_none());
    }

    #[test]
    fn select_all_and_delete() {
        let mut t = typed("everything");
        t.select_all();
        assert_eq!(t.selected_text().as_deref(), Some("everything"));
        t.backspace();
        assert!(t.is_empty());
    }

    #[test]
    fn plain_move_collapses_selection() {
        let mut t = typed("abcdef");
        t.home(false);
        t.right(true);
        t.right(true); // select "ab"
        assert_eq!(t.selection(), Some((0, 2)));
        t.left(false); // collapse to left edge
        assert_eq!(t.cursor(), 0);
        assert!(t.selection().is_none());
    }

    #[test]
    fn delete_word_back() {
        let mut t = typed("one two three");
        t.delete_word_back();
        assert_eq!(t.text(), "one two ");
    }
}
