use std::collections::VecDeque;
use std::mem;

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;

use super::{EntryKind, TranscriptEntry, formatting};

pub(super) struct StreamingText {
    kind: EntryKind,
    width: u16,
    source_len: usize,
    stable_lines: Vec<Line<'static>>,
    preview_lines: Vec<Line<'static>>,
    state: WrapState,
    tail: String,
    label_style: Style,
}

#[derive(Clone)]
struct WrapState {
    width: usize,
    pending_line: Vec<Glyph>,
    pending_word: Vec<Glyph>,
    pending_whitespace: VecDeque<Glyph>,
    line_width: usize,
    word_width: usize,
    whitespace_width: usize,
    non_whitespace_previous: bool,
    logical_has_lines: bool,
}

#[derive(Clone)]
struct Glyph {
    text: String,
    style: Style,
    width: usize,
    whitespace: bool,
}

impl StreamingText {
    pub(super) fn new(source: &TranscriptEntry, width: u16) -> Self {
        let (label, label_style, _) = formatting::label(source.kind);
        let mut text = Self {
            kind: source.kind,
            width: width.max(1),
            source_len: 0,
            stable_lines: Vec::new(),
            preview_lines: Vec::new(),
            state: WrapState::new(width),
            tail: String::new(),
            label_style,
        };
        push_text(
            &mut text.state,
            &mut text.stable_lines,
            &format!("{label:<11}"),
            label_style,
        );
        text.append_delta(&source.text);
        text.source_len = source.text.len();
        text
    }

    pub(super) fn update(&mut self, source: &TranscriptEntry, width: u16) -> usize {
        let appended = source.text.len().saturating_sub(self.source_len);
        if self.kind != source.kind
            || self.width != width.max(1)
            || source.text.len() < self.source_len
        {
            *self = Self::new(source, width);
            return appended;
        }
        let delta = &source.text[self.source_len..];
        if !delta.is_empty() {
            self.append_delta(delta);
            self.source_len = source.text.len();
        }
        appended
    }

    pub(super) fn line_count(&self) -> usize {
        self.stable_lines.len() + self.preview_lines.len()
    }

    pub(super) fn render(&self, area: Rect, buffer: &mut Buffer, top: usize) {
        for (row, line) in self
            .stable_lines
            .iter()
            .chain(&self.preview_lines)
            .skip(top)
            .take(usize::from(area.height))
            .enumerate()
        {
            let y = area
                .y
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
            line.render(Rect::new(area.x, y, area.width, 1), buffer);
        }
    }

    fn append_delta(&mut self, delta: &str) {
        let mut combined = mem::take(&mut self.tail);
        combined.push_str(delta);
        let mut graphemes = combined.graphemes(true).peekable();
        while let Some(grapheme) = graphemes.next() {
            if graphemes.peek().is_none() {
                self.tail.push_str(grapheme);
            } else {
                process_unit(
                    &mut self.state,
                    &mut self.stable_lines,
                    grapheme,
                    self.label_style,
                );
            }
        }
        self.rebuild_preview();
    }

    fn rebuild_preview(&mut self) {
        let mut state = self.state.clone();
        let mut preview = Vec::new();
        if !self.tail.is_empty() && !is_newline(&self.tail) {
            process_unit(&mut state, &mut preview, &self.tail, self.label_style);
        }
        state.finish_logical(&mut preview);
        self.preview_lines = preview;
    }
}

impl WrapState {
    fn new(width: u16) -> Self {
        Self {
            width: usize::from(width.max(1)),
            pending_line: Vec::new(),
            pending_word: Vec::new(),
            pending_whitespace: VecDeque::new(),
            line_width: 0,
            word_width: 0,
            whitespace_width: 0,
            non_whitespace_previous: false,
            logical_has_lines: false,
        }
    }

    fn push(&mut self, glyph: Glyph, output: &mut Vec<Line<'static>>) {
        if glyph.width > self.width {
            return;
        }
        let whitespace = glyph.whitespace;
        let word_found = self.non_whitespace_previous && whitespace;
        let untrimmed_overflow = self.pending_line.is_empty()
            && self
                .word_width
                .saturating_add(self.whitespace_width)
                .saturating_add(glyph.width)
                > self.width;
        if word_found || untrimmed_overflow {
            self.flush_pending_to_line();
        }
        let line_full = self.line_width >= self.width;
        let word_overflow = glyph.width > 0
            && self
                .line_width
                .saturating_add(self.whitespace_width)
                .saturating_add(self.word_width)
                >= self.width;
        if line_full || word_overflow {
            let mut remaining = self.width.saturating_sub(self.line_width);
            self.emit_line(output);
            while let Some(glyph) = self.pending_whitespace.front() {
                if glyph.width > remaining {
                    break;
                }
                self.whitespace_width -= glyph.width;
                remaining -= glyph.width;
                let _ = self.pending_whitespace.pop_front();
            }
            if whitespace && self.pending_whitespace.is_empty() {
                return;
            }
        }
        if whitespace {
            self.whitespace_width += glyph.width;
            self.pending_whitespace.push_back(glyph);
        } else {
            self.word_width += glyph.width;
            self.pending_word.push(glyph);
        }
        self.non_whitespace_previous = !whitespace;
    }

    fn finish_logical(&mut self, output: &mut Vec<Line<'static>>) {
        self.flush_pending_to_line();
        if !self.pending_line.is_empty() {
            self.emit_line(output);
        } else if !self.logical_has_lines {
            output.push(Line::default());
        }
        self.reset_logical();
    }

    fn flush_pending_to_line(&mut self) {
        self.pending_line.extend(self.pending_whitespace.drain(..));
        self.line_width += self.whitespace_width;
        self.pending_line.append(&mut self.pending_word);
        self.line_width += self.word_width;
        self.whitespace_width = 0;
        self.word_width = 0;
    }

    fn emit_line(&mut self, output: &mut Vec<Line<'static>>) {
        output.push(glyphs_to_line(mem::take(&mut self.pending_line)));
        self.line_width = 0;
        self.logical_has_lines = true;
    }

    fn reset_logical(&mut self) {
        self.pending_line.clear();
        self.pending_word.clear();
        self.pending_whitespace.clear();
        self.line_width = 0;
        self.word_width = 0;
        self.whitespace_width = 0;
        self.non_whitespace_previous = false;
        self.logical_has_lines = false;
    }
}

fn process_unit(
    state: &mut WrapState,
    output: &mut Vec<Line<'static>>,
    grapheme: &str,
    label_style: Style,
) {
    if is_newline(grapheme) {
        state.finish_logical(output);
        push_text(state, output, "           ", label_style);
    } else if !grapheme.contains(char::is_control) {
        state.push(Glyph::new(grapheme, Style::default()), output);
    }
}

fn is_newline(grapheme: &str) -> bool {
    matches!(grapheme, "\n" | "\r\n")
}

fn push_text(state: &mut WrapState, output: &mut Vec<Line<'static>>, text: &str, style: Style) {
    for grapheme in text.graphemes(true) {
        state.push(Glyph::new(grapheme, style), output);
    }
}

impl Glyph {
    fn new(text: &str, style: Style) -> Self {
        Self {
            text: text.to_string(),
            style,
            width: usize::from(text.cell_width()),
            whitespace: text == "\u{200b}"
                || text.chars().all(char::is_whitespace) && text != "\u{00a0}",
        }
    }
}

fn glyphs_to_line(glyphs: Vec<Glyph>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for glyph in glyphs {
        if let Some(span) = spans.last_mut()
            && span.style == glyph.style
        {
            span.content.to_mut().push_str(&glyph.text);
        } else {
            spans.push(Span::styled(glyph.text, glyph.style));
        }
    }
    Line::from(spans)
}
