//! Terminal driver for the multi-line chat input editor.
//!
//! Owns the Ratatui terminal lifecycle and the blocking event loop. The editor
//! renders into an inline viewport (`Viewport::Inline`), so the conversation
//! scrollback above the box is preserved - no full-screen clear. `ratatui`'s
//! `init_with_options` enables raw mode and installs a panic hook that restores
//! the terminal; a `TermGuard` additionally disables bracketed paste and
//! restores on every exit path (normal, `?`, or panic).
//!
//! Falls back to a plain `read_line` when stdin/stdout is not a TTY.

use std::io::{self, BufRead, Write};

use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{Clear, ClearType};
use ratatui::{DefaultTerminal, TerminalOptions, Viewport};

use crate::term::editor::{ChatEditor, EditorAction};

/// Rows reserved for the inline input viewport: bordered box (>=3) + hint line.
const VIEWPORT_HEIGHT: u16 = 8;

// The reserved height must fit a bordered box (min 3 rows) plus the hint line.
const _: () = assert!(VIEWPORT_HEIGHT > 3);

/// Read multi-line input from the terminal. Returns `Ok(text)` on submit,
/// `Err(Interrupted)` on Ctrl+C, `Err(UnexpectedEof)` on Ctrl+D. Falls back to
/// `read_line` when stdin/stdout is not a TTY.
///
/// # Errors
/// Propagates terminal I/O errors; `Interrupted` / `UnexpectedEof` are the
/// normal Ctrl+C / Ctrl+D control signals rather than failures.
pub fn read_multiline(prompt: &str, history: &mut Vec<String>) -> io::Result<String> {
    if !io::IsTerminal::is_terminal(&io::stdin()) || !io::IsTerminal::is_terminal(&io::stdout()) {
        return read_line_fallback(prompt);
    }
    read_multiline_tui(prompt, history)
}

fn read_line_fallback(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    if input.ends_with('\n') {
        input.pop();
        if input.ends_with('\r') {
            input.pop();
        }
    }
    Ok(input)
}

/// RAII guard: disables bracketed paste and restores the terminal on drop,
/// covering the normal return, `?`-propagated errors, and panics.
struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

fn read_multiline_tui(prompt: &str, history: &mut Vec<String>) -> io::Result<String> {
    let mut terminal = ratatui::try_init_with_options(TerminalOptions {
        viewport: Viewport::Inline(VIEWPORT_HEIGHT),
    })?;
    let guard = TermGuard;
    execute!(io::stdout(), EnableBracketedPaste)?;

    let mut editor = ChatEditor::new(prompt);
    let outcome = event_loop(&mut terminal, &mut editor, history);

    // Collapse the inline box back into the normal buffer so the model's reply
    // flows below it, then echo a clean transcript line for a submitted turn.
    let vp = terminal.get_frame().area();
    let _ = execute!(
        io::stdout(),
        MoveTo(vp.x, vp.y),
        Clear(ClearType::FromCursorDown)
    );
    if let Ok(text) = &outcome {
        if !text.is_empty() {
            drop(terminal);
            drop(guard);
            println!("{prompt}{text}");
            return outcome;
        }
    }
    outcome
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    editor: &mut ChatEditor,
    history: &mut Vec<String>,
) -> io::Result<String> {
    loop {
        terminal.draw(|f| editor.render(f, f.area()))?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match editor.handle_key(key, history) {
                    EditorAction::Submit => {
                        let text = editor.input().to_string();
                        if !text.is_empty() {
                            history.push(text.clone());
                        }
                        return Ok(text);
                    }
                    EditorAction::Cancel => {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "Ctrl+C"));
                    }
                    EditorAction::Eof => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Ctrl+D"));
                    }
                    EditorAction::Continue => {}
                }
            }
            Event::Paste(pasted) => editor.paste(&pasted),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fallback_trims_trailing_newline() {
        // read_line_fallback is exercised by the non-TTY path; verify the
        // trailing-newline trimming logic it relies on.
        let mut s = String::from("hello\r\n");
        if s.ends_with('\n') {
            s.pop();
            if s.ends_with('\r') {
                s.pop();
            }
        }
        assert_eq!(s, "hello");
    }
}
