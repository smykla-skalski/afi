use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

use crate::term::{MessageKind, StreamKind};

use super::{EntryKind, TranscriptEntry};

pub(super) fn entry(source: &TranscriptEntry) -> (Paragraph<'static>, bool) {
    let (label, label_style, supports_markdown) = label(source.kind);
    let markdown = supports_markdown && !source.streaming;
    let content = if markdown {
        owned_lines(tui_markdown::from_str(&source.text))
    } else {
        Text::from(source.text.clone()).lines
    };
    let content = if content.is_empty() {
        vec![Line::default()]
    } else {
        content
    };
    let lines = content
        .into_iter()
        .enumerate()
        .map(|(index, line)| prefix_line(line, label, label_style, index == 0))
        .collect::<Vec<_>>();
    (
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        markdown,
    )
}

fn prefix_line(line: Line<'static>, label: &str, label_style: Style, first: bool) -> Line<'static> {
    let prefix = if first {
        format!("{label:<11}")
    } else {
        " ".repeat(11)
    };
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::styled(prefix, label_style));
    spans.extend(line.spans);
    Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    }
}

fn owned_lines(text: Text<'_>) -> Vec<Line<'static>> {
    text.lines
        .into_iter()
        .map(|line| Line {
            style: line.style,
            alignment: line.alignment,
            spans: line
                .spans
                .into_iter()
                .map(|span| Span {
                    style: span.style,
                    content: Cow::Owned(span.content.into_owned()),
                })
                .collect(),
        })
        .collect()
}

pub(super) fn label(kind: EntryKind) -> (&'static str, Style, bool) {
    let normal = Style::default();
    match kind {
        EntryKind::User => ("you", normal.fg(Color::Cyan).bold(), false),
        EntryKind::Message(MessageKind::Info) => ("info", normal.fg(Color::Gray), false),
        EntryKind::Message(MessageKind::Warning) => ("warning", normal.fg(Color::Yellow), false),
        EntryKind::Message(MessageKind::Error) => ("error", normal.fg(Color::Red).bold(), false),
        EntryKind::Message(MessageKind::Stats) => ("stats", normal.fg(Color::DarkGray), false),
        EntryKind::Message(MessageKind::Tool) => ("tool", normal.fg(Color::Magenta), false),
        EntryKind::Stream(StreamKind::Assistant) => {
            ("assistant", normal.fg(Color::Green).bold(), true)
        }
        EntryKind::Stream(StreamKind::Reasoning) => (
            "reasoning",
            normal.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            false,
        ),
    }
}
