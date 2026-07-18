//! Interactive tool-approval prompt.
//!
//! `prompt_choice` asks the user to approve a risky action, reading a single
//! keypress in raw mode: `y` approves, `n`/Enter denies, Esc (or Ctrl+C) drops
//! back to chat. It is passed to `risk::confirm` as the `ask` callback, so the
//! approval gate is a real interaction rather than a hardwired "yes".
//!
//! When stdin/stdout is not a TTY there is no way to prompt, so the safe
//! default is to deny.

use std::io::{self, IsTerminal, Write};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::risk::ApprovalChoice;

/// Prompt the user to approve `action`. Returns their choice; denies when no
/// TTY is available.
pub fn prompt_choice(action: &str) -> ApprovalChoice {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return ApprovalChoice::No;
    }
    print!("\r\x1b[33m  approve {}? [y/N/esc]\x1b[0m ", action);
    let _ = io::stdout().flush();
    let choice = read_choice().unwrap_or(ApprovalChoice::No);
    println!();
    choice
}

/// Restores cooked mode on drop so a panic never leaves the terminal raw.
struct RawGuard;

impl RawGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn read_choice() -> io::Result<ApprovalChoice> {
    let _raw = RawGuard::enter()?;
    loop {
        if let Event::Key(key) = event::read()? {
            if let Some(choice) = classify_key(key) {
                return Ok(choice);
            }
        }
    }
}

/// Map a keypress to a decision, or `None` to keep waiting. `y` approves;
/// `n` or Enter denies; Esc or Ctrl+C escapes to chat.
fn classify_key(key: KeyEvent) -> Option<ApprovalChoice> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalChoice::Yes),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ApprovalChoice::No),
        KeyCode::Enter => Some(ApprovalChoice::No),
        KeyCode::Esc => Some(ApprovalChoice::Esc),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(ApprovalChoice::Esc)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn y_approves_n_and_enter_deny() {
        assert_eq!(
            classify_key(key(KeyCode::Char('y'))),
            Some(ApprovalChoice::Yes)
        );
        assert_eq!(
            classify_key(key(KeyCode::Char('Y'))),
            Some(ApprovalChoice::Yes)
        );
        assert_eq!(
            classify_key(key(KeyCode::Char('n'))),
            Some(ApprovalChoice::No)
        );
        assert_eq!(classify_key(key(KeyCode::Enter)), Some(ApprovalChoice::No));
    }

    #[test]
    fn esc_and_ctrl_c_escape() {
        assert_eq!(classify_key(key(KeyCode::Esc)), Some(ApprovalChoice::Esc));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(classify_key(ctrl_c), Some(ApprovalChoice::Esc));
    }

    #[test]
    fn other_keys_keep_waiting() {
        assert_eq!(classify_key(key(KeyCode::Char('x'))), None);
        assert_eq!(classify_key(key(KeyCode::Left)), None);
    }

    #[test]
    fn release_events_are_ignored() {
        let mut ev = key(KeyCode::Char('y'));
        ev.kind = KeyEventKind::Release;
        assert_eq!(classify_key(ev), None);
    }

    #[test]
    fn non_tty_denies() {
        // Under `cargo test` stdout is not a TTY, so no prompt is shown.
        assert_eq!(prompt_choice("run: rm -rf /"), ApprovalChoice::No);
    }
}
