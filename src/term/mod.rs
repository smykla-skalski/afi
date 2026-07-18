//! Terminal UI: ANSI codes, OSC title, char width helpers, Life spinner,
//! chatbox input editor, and the Esc interrupt watcher.
//!
//! The `chatbox` driver and the `editor` state render the multi-line input
//! with Ratatui (inline viewport, bordered box, real cursor). The ANSI
//! constants and width helpers below serve the remaining non-Ratatui output
//! (banner, streamed model tokens, Life spinner).

pub mod chatbox;
pub mod editor;
pub mod interrupt;
pub mod life;

// ANSI escape codes (a small subset for non-ratatui output).
pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";
pub const CLEAR_LINE: &str = "\x1b[2K\r";
pub const HIDE_CURSOR: &str = "\x1b[?25l";
pub const SHOW_CURSOR: &str = "\x1b[?25h";

// --- OSC terminal title ------------------------------------------------------

/// Set the terminal-tab title via OSC 0.
pub fn set_title(text: &str) {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        print!("\x1b]0;{}\x07", text);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
}

/// Set the idle title: "afi idle".
pub fn set_idle_title() {
    set_title("afi idle");
}

/// Set the working title with a spinner glyph.
pub fn set_working_title(frame: usize) {
    let frames = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";
    let glyph = frames
        .chars()
        .nth(frame % frames.chars().count())
        .unwrap_or('⠋');
    set_title(&format!("{} afi working", glyph));
}

// --- char width helpers ------------------------------------------------------

use unicode_width::UnicodeWidthChar;

/// Display width of a character (0 for combining, 2 for wide CJK).
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Display width of a string.
pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Slice a string by display width (returns the longest prefix that fits).
pub fn str_slice_by_width(s: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut result = String::new();
    for ch in s.chars() {
        let w = char_width(ch);
        if width + w > max_width {
            break;
        }
        width += w;
        result.push(ch);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_width_ascii() {
        assert_eq!(str_width("hello"), 5);
    }

    #[test]
    fn str_width_cjk() {
        assert_eq!(str_width("你好"), 4); // 2 wide chars * 2 = 4
    }

    #[test]
    fn str_slice_by_width_clamps() {
        assert_eq!(str_slice_by_width("hello world", 5), "hello");
    }

    #[test]
    fn str_slice_by_width_cjk() {
        // Each CJK char is width 2, so max_width=3 gives 1 char.
        assert_eq!(str_slice_by_width("你好世界", 3), "你");
    }
}
