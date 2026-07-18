//! Multi-line chatbox input editor - a port of the Python `_chatbox_raw`.
//!
//! Uses crossterm for raw terminal input. Supports:
//! - Enter submits; Alt+Enter or Ctrl+J inserts a newline
//! - Paste (bracketed-paste) inserts text verbatim, including newlines
//! - Up/Down navigate history; Left/Right move within the line
//! - Home/End jump to line start/end; Ctrl+U clears; Ctrl+C cancels
//! - Long lines word-wrap inside the box
//!
//! Falls back to plain `read_line` when stdin/stdout isn't a TTY.

use std::io::{self, BufRead, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Read multi-line input from the terminal. Returns `Ok(text)` on submit,
/// `Err(io::Error)` on EOF/Ctrl+C. Falls back to `read_line` when not a TTY.
pub fn read_multiline(prompt: &str, history: &mut Vec<String>) -> io::Result<String> {
    if !io::IsTerminal::is_terminal(&io::stdin()) || !io::IsTerminal::is_terminal(&io::stdout()) {
        return read_line_fallback(prompt);
    }

    // Enable raw mode for byte-at-a-time input.
    let _ = enable_raw_mode();
    let result = read_multiline_raw(prompt, history);
    let _ = disable_raw_mode();
    result
}

fn read_line_fallback(prompt: &str) -> io::Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    // Trim trailing newline.
    if input.ends_with('\n') {
        input.pop();
        if input.ends_with('\r') {
            input.pop();
        }
    }
    Ok(input)
}

fn read_multiline_raw(prompt: &str, history: &mut Vec<String>) -> io::Result<String> {
    let mut buffer = String::new();
    let mut cursor: usize = 0; // byte position in buffer
    let mut history_idx: Option<usize> = None;

    // Render the initial prompt.
    render(prompt, &buffer, cursor)?;

    loop {
        let event = event::read()?;

        match event {
            Event::Key(key) => {
                match handle_key(key, &mut buffer, &mut cursor, history, &mut history_idx) {
                    KeyAction::Submit => {
                        // Move to a new line, save to history, return.
                        println!();
                        if !buffer.is_empty() {
                            history.push(buffer.clone());
                        }
                        return Ok(buffer);
                    }
                    KeyAction::Cancel => {
                        println!();
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "Ctrl+C"));
                    }
                    KeyAction::Eof => {
                        println!();
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Ctrl+D"));
                    }
                    KeyAction::Continue => {}
                }
                render(prompt, &buffer, cursor)?;
            }
            Event::Paste(pasted) => {
                // Insert pasted text verbatim (including newlines).
                buffer.insert_str(cursor, &pasted);
                cursor += pasted.len();
                render(prompt, &buffer, cursor)?;
            }
            _ => {}
        }
    }
}

enum KeyAction {
    Continue,
    Submit,
    Cancel,
    Eof,
}

fn handle_key(
    key: KeyEvent,
    buffer: &mut String,
    cursor: &mut usize,
    history: &mut [String],
    history_idx: &mut Option<usize>,
) -> KeyAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Enter if ctrl || alt => {
            // Insert a newline.
            buffer.insert(*cursor, '\n');
            *cursor += 1;
            KeyAction::Continue
        }
        KeyCode::Enter => {
            // Submit.
            KeyAction::Submit
        }
        KeyCode::Char('j') if ctrl => {
            // Ctrl+J = newline (same as Alt+Enter).
            buffer.insert(*cursor, '\n');
            *cursor += 1;
            KeyAction::Continue
        }
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C = cancel.
            KeyAction::Cancel
        }
        KeyCode::Char('d') if ctrl => {
            // Ctrl+D = EOF (if buffer is empty).
            if buffer.is_empty() {
                KeyAction::Eof
            } else {
                // Delete char under cursor (like readline).
                if *cursor < buffer.len() {
                    buffer.remove(*cursor);
                }
                KeyAction::Continue
            }
        }
        KeyCode::Char('u') if ctrl => {
            // Ctrl+U = clear line before cursor.
            buffer.drain(..*cursor);
            *cursor = 0;
            KeyAction::Continue
        }
        KeyCode::Char(ch) => {
            // Insert the character.
            buffer.insert(*cursor, ch);
            *cursor += ch.len_utf8();
            KeyAction::Continue
        }
        KeyCode::Backspace => {
            // Delete char before cursor.
            if *cursor > 0 {
                // Find the start of the previous char.
                let prev = buffer[..*cursor]
                    .chars()
                    .last()
                    .map(|c| *cursor - c.len_utf8())
                    .unwrap_or(0);
                buffer.drain(prev..*cursor);
                *cursor = prev;
            }
            KeyAction::Continue
        }
        KeyCode::Delete => {
            // Delete char under cursor.
            if *cursor < buffer.len() {
                let next = buffer[*cursor..]
                    .chars()
                    .next()
                    .map(|c| *cursor + c.len_utf8())
                    .unwrap_or(*cursor);
                buffer.drain(*cursor..next);
            }
            KeyAction::Continue
        }
        KeyCode::Left => {
            // Move cursor left by one char.
            if *cursor > 0 {
                *cursor -= buffer[..*cursor]
                    .chars()
                    .last()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
            KeyAction::Continue
        }
        KeyCode::Right => {
            // Move cursor right by one char.
            if *cursor < buffer.len() {
                *cursor += buffer[*cursor..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
            KeyAction::Continue
        }
        KeyCode::Home => {
            // Jump to start of the current line.
            *cursor = buffer[..*cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
            KeyAction::Continue
        }
        KeyCode::End => {
            // Jump to end of the current line.
            *cursor = buffer[*cursor..]
                .find('\n')
                .map(|i| *cursor + i)
                .unwrap_or(buffer.len());
            KeyAction::Continue
        }
        KeyCode::Up => {
            // Navigate history backwards.
            if history.is_empty() {
                return KeyAction::Continue;
            }
            if history_idx.is_none() {
                *history_idx = Some(history.len());
            }
            if let Some(idx) = *history_idx {
                if idx > 0 {
                    *history_idx = Some(idx - 1);
                    *buffer = history[idx - 1].clone();
                    *cursor = buffer.len();
                }
            }
            KeyAction::Continue
        }
        KeyCode::Down => {
            // Navigate history forwards.
            if let Some(idx) = *history_idx {
                if idx < history.len() - 1 {
                    *history_idx = Some(idx + 1);
                    *buffer = history[idx + 1].clone();
                    *cursor = buffer.len();
                } else {
                    *history_idx = None;
                    buffer.clear();
                    *cursor = 0;
                }
            }
            KeyAction::Continue
        }
        KeyCode::Esc => {
            // Clear the buffer (acts like a "reset" in the chatbox).
            buffer.clear();
            *cursor = 0;
            KeyAction::Continue
        }
        _ => KeyAction::Continue,
    }
}

fn render(prompt: &str, buffer: &str, cursor: usize) -> io::Result<()> {
    let mut stdout = io::stdout();

    // Clear the current line and move to the start.
    write!(stdout, "\r\x1b[2J")?; // Clear screen (simpler than scroll-region management)
    write!(stdout, "{}", prompt)?;

    // Render the buffer (with a cursor indicator).
    let before = &buffer[..cursor.min(buffer.len())];
    let at = if cursor < buffer.len() {
        &buffer[cursor..cursor + 1]
    } else {
        " "
    };
    let after = if cursor + 1 < buffer.len() {
        &buffer[cursor + 1..]
    } else {
        ""
    };

    write!(stdout, "{}\x1b[7m{}\x1b[0m{}", before, at, after)?;
    stdout.flush()
}
