//! Multi-line chat input editor state, rendered with Ratatui widgets.
//!
//! `ChatEditor` owns the edit buffer, a byte-index cursor, and history
//! navigation state. It is pure and toolkit-testable: `handle_key` and
//! `paste` mutate state, and `render` draws a bordered, wrapping input box
//! with the real terminal cursor placed via Ratatui's cursor API. The driver
//! in `chatbox.rs` owns the terminal lifecycle and the event loop.
//!
//! Rendering wraps the buffer into visual rows at display-width boundaries and
//! computes the cursor's (row, col) in that wrapped space, so multibyte and
//! wide (CJK) glyphs never split - the old raw-ANSI editor byte-sliced at
//! `cursor + 1` and panicked on any non-ASCII character.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::term::char_width;
use std::mem;

/// What the driver should do after a key was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    Continue,
    Submit,
    Cancel,
    Eof,
}

/// Multi-line input editor state.
pub struct ChatEditor {
    buffer: String,
    cursor: usize,
    history_idx: Option<usize>,
    title: String,
}

impl ChatEditor {
    /// Create an empty editor. `title` is shown on the box border.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history_idx: None,
            title: title.trim().to_string(),
        }
    }

    /// The current buffer contents.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.buffer
    }

    /// Insert pasted text verbatim at the cursor (bracketed paste).
    pub fn paste(&mut self, text: &str) {
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Apply a key event, returning what the driver should do next.
    pub fn handle_key(&mut self, key: KeyEvent, history: &[String]) -> EditorAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if ctrl || alt => self.insert_char('\n'),
            KeyCode::Enter => return EditorAction::Submit,
            KeyCode::Char('j') if ctrl => self.insert_char('\n'),
            KeyCode::Char('c') if ctrl => return EditorAction::Cancel,
            KeyCode::Char('d') if ctrl => {
                if self.buffer.is_empty() {
                    return EditorAction::Eof;
                }
                self.delete_under();
            }
            KeyCode::Char('u') if ctrl => {
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_under(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up => self.history_prev(history),
            KeyCode::Down => self.history_next(history),
            KeyCode::Esc => {
                self.buffer.clear();
                self.cursor = 0;
            }
            _ => {}
        }
        EditorAction::Continue
    }

    fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.prev_boundary(self.cursor);
            self.buffer.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    fn delete_under(&mut self) {
        if self.cursor < self.buffer.len() {
            let next = self.next_boundary(self.cursor);
            self.buffer.drain(self.cursor..next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_boundary(self.cursor);
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor = self.next_boundary(self.cursor);
        }
    }

    fn prev_boundary(&self, i: usize) -> usize {
        self.buffer[..i]
            .chars()
            .next_back()
            .map_or(0, |c| i - c.len_utf8())
    }

    fn next_boundary(&self, i: usize) -> usize {
        self.buffer[i..]
            .chars()
            .next()
            .map_or(i, |c| i + c.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.buffer[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.buffer[self.cursor..]
            .find('\n')
            .map_or(self.buffer.len(), |i| self.cursor + i)
    }

    fn history_prev(&mut self, history: &[String]) {
        if history.is_empty() {
            return;
        }
        let idx = self.history_idx.unwrap_or(history.len());
        if idx > 0 {
            self.set_from_history(history, idx - 1);
        }
    }

    fn history_next(&mut self, history: &[String]) {
        if let Some(idx) = self.history_idx {
            if idx + 1 < history.len() {
                self.set_from_history(history, idx + 1);
            } else {
                self.history_idx = None;
                self.buffer.clear();
                self.cursor = 0;
            }
        }
    }

    fn set_from_history(&mut self, history: &[String], idx: usize) {
        self.history_idx = Some(idx);
        self.buffer.clone_from(&history[idx]);
        self.cursor = self.buffer.len();
    }

    /// Draw the bordered, wrapping input box plus a dim hint line, and place
    /// the real terminal cursor at the edit point.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let [box_area, hint_area] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);

        let block = Block::bordered().title(self.title.as_str());
        let inner = block.inner(box_area);
        let width = inner.width.max(1) as usize;
        let height = inner.height.max(1) as usize;

        let (rows, cur_row, cur_col) = wrap(&self.buffer, self.cursor, width);
        let scroll = u16::try_from(cur_row.saturating_sub(height - 1)).unwrap_or(u16::MAX);
        let text = Text::from(rows.into_iter().map(Line::from).collect::<Vec<_>>());
        frame.render_widget(
            Paragraph::new(text).block(block).scroll((scroll, 0)),
            box_area,
        );

        let hint = "Enter submit \u{b7} Alt+Enter newline \u{b7} Ctrl+U clear \u{b7} Ctrl+C cancel";
        frame.render_widget(Paragraph::new(hint.dim()), hint_area);

        let cx = inner
            .x
            .saturating_add(u16::try_from(cur_col).unwrap_or(u16::MAX))
            .min(inner.right().saturating_sub(1));
        let cy = inner
            .y
            .saturating_add(
                u16::try_from(cur_row)
                    .unwrap_or(u16::MAX)
                    .saturating_sub(scroll),
            )
            .min(inner.bottom().saturating_sub(1));
        frame.set_cursor_position((cx, cy));
    }
}

/// Wrap `buffer` into visual rows of at most `width` display columns, returning
/// the rows plus the cursor's `(row, col)` in wrapped space. Wrapping happens
/// at display-width boundaries, so wide glyphs are never split.
fn wrap(buffer: &str, cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_w = 0usize;
    let mut cur_row = 0usize;
    let mut cur_col = 0usize;
    let mut found = false;

    for (b, c) in buffer.char_indices() {
        if c != '\n' {
            let w = char_width(c);
            if line_w + w > width && line_w > 0 {
                rows.push(mem::take(&mut line));
                line_w = 0;
            }
        }
        if b == cursor && !found {
            cur_row = rows.len();
            cur_col = line_w;
            found = true;
        }
        if c == '\n' {
            rows.push(mem::take(&mut line));
            line_w = 0;
        } else {
            line.push(c);
            line_w += char_width(c);
        }
    }
    if !found {
        cur_row = rows.len();
        cur_col = line_w;
    }
    rows.push(line);
    (rows, cur_row, cur_col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(ed: &mut ChatEditor, s: &str) {
        for c in s.chars() {
            ed.handle_key(press(KeyCode::Char(c)), &[]);
        }
    }

    #[test]
    fn typing_and_submit() {
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "hello");
        assert_eq!(ed.input(), "hello");
        assert_eq!(
            ed.handle_key(press(KeyCode::Enter), &[]),
            EditorAction::Submit
        );
    }

    #[test]
    fn alt_enter_inserts_newline_not_submit() {
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "a");
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(ed.handle_key(alt_enter, &[]), EditorAction::Continue);
        type_str(&mut ed, "b");
        assert_eq!(ed.input(), "a\nb");
    }

    #[test]
    fn ctrl_c_cancels_ctrl_d_eof_on_empty() {
        let mut ed = ChatEditor::new("> ");
        assert_eq!(ed.handle_key(ctrl('c'), &[]), EditorAction::Cancel);
        assert_eq!(ed.handle_key(ctrl('d'), &[]), EditorAction::Eof);
    }

    #[test]
    fn ctrl_d_deletes_when_not_empty() {
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "ab");
        ed.handle_key(press(KeyCode::Left), &[]);
        assert_eq!(ed.handle_key(ctrl('d'), &[]), EditorAction::Continue);
        assert_eq!(ed.input(), "a");
    }

    #[test]
    fn multibyte_left_then_render_does_not_panic() {
        // Regression: the old editor sliced `&buffer[cursor..cursor+1]` and
        // panicked here on the 2-byte 'e-acute'. Moving left must be safe.
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "\u{e9}"); // é
        ed.handle_key(press(KeyCode::Left), &[]);
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();
        terminal
            .draw(|f| ed.render(f, f.area()))
            .expect("render must not panic on a multibyte buffer");
        let pos = terminal.get_cursor_position().unwrap();
        // Cursor sits before the 'é', i.e. at the first inner column (x=1, y=1).
        assert_eq!((pos.x, pos.y), (1, 1));
    }

    #[test]
    fn backspace_removes_whole_multibyte_char() {
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "a\u{4f60}"); // a + CJK '你'
        ed.handle_key(press(KeyCode::Backspace), &[]);
        assert_eq!(ed.input(), "a");
    }

    #[test]
    fn wide_char_advances_cursor_two_columns() {
        // '你' has display width 2, so after typing it the cursor is 2 cols in.
        let (rows, row, col) = wrap("\u{4f60}", "\u{4f60}".len(), 10);
        assert_eq!(rows, vec!["\u{4f60}".to_string()]);
        assert_eq!((row, col), (0, 2));
    }

    #[test]
    fn wrap_splits_at_width_boundary() {
        let (rows, row, col) = wrap("abcde", 5, 3);
        assert_eq!(rows, vec!["abc".to_string(), "de".to_string()]);
        assert_eq!((row, col), (1, 2));
    }

    #[test]
    fn wrap_keeps_explicit_newlines() {
        let (rows, row, col) = wrap("ab\ncd", 5, 10);
        assert_eq!(rows, vec!["ab".to_string(), "cd".to_string()]);
        assert_eq!((row, col), (1, 2));
    }

    #[test]
    fn home_end_move_within_line() {
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "one\ntwo");
        ed.handle_key(press(KeyCode::Home), &[]);
        assert_eq!(ed.cursor, 4); // start of "two"
        ed.handle_key(press(KeyCode::End), &[]);
        assert_eq!(ed.cursor, 7); // end of buffer
    }

    #[test]
    fn history_up_down_navigates() {
        let history = vec!["first".to_string(), "second".to_string()];
        let mut ed = ChatEditor::new("> ");
        ed.handle_key(press(KeyCode::Up), &history);
        assert_eq!(ed.input(), "second");
        ed.handle_key(press(KeyCode::Up), &history);
        assert_eq!(ed.input(), "first");
        ed.handle_key(press(KeyCode::Down), &history);
        assert_eq!(ed.input(), "second");
        ed.handle_key(press(KeyCode::Down), &history);
        assert_eq!(ed.input(), ""); // past newest clears
    }

    #[test]
    fn paste_inserts_verbatim_including_newlines() {
        let mut ed = ChatEditor::new("> ");
        type_str(&mut ed, "ab");
        ed.handle_key(press(KeyCode::Left), &[]);
        ed.paste("X\nY");
        assert_eq!(ed.input(), "aX\nYb");
    }

    #[test]
    fn render_draws_border_and_hint() {
        let mut ed = ChatEditor::new("prompt");
        type_str(&mut ed, "hi");
        let mut terminal = Terminal::new(TestBackend::new(24, 6)).unwrap();
        terminal.draw(|f| ed.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        let top: String = (0..24).map(|x| buf[(x, 0)].symbol().to_owned()).collect();
        assert!(top.contains("prompt"), "border title missing: {top:?}");
        let hint: String = (0..24).map(|x| buf[(x, 5)].symbol().to_owned()).collect();
        assert!(hint.contains("Enter submit"), "hint missing: {hint:?}");
    }
}
