use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use throbber_widgets_tui::{BRAILLE_SIX, Throbber};

use crate::term::{MessageKind, StreamKind};

use super::app::{EntryKind, TranscriptEntry, TuiApp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutAreas {
    pub(super) header: Rect,
    pub(super) transcript: Rect,
    pub(super) status: Rect,
    pub(super) composer: Rect,
    pub(super) footer: Rect,
}

pub(super) fn render(frame: &mut Frame, app: &mut TuiApp) {
    let areas = layout_areas(frame.area(), app.composer_height());
    render_header(frame, app, areas.header);
    render_transcript(frame, app, areas.transcript);
    render_status(frame, app, areas.status);
    frame.render_widget(&app.composer, areas.composer);
    render_footer(frame, app, areas.footer);
    if app.approval.is_some() {
        render_approval(frame, app);
    }
}

pub(super) fn layout_areas(area: Rect, composer_height: u16) -> LayoutAreas {
    let heights = layout_heights(area.height, composer_height);
    let mut y = area.y;
    let rects = heights.map(|height| {
        let rect = Rect::new(area.x, y, area.width, height);
        y = y.saturating_add(height);
        rect
    });
    LayoutAreas {
        header: rects[0],
        transcript: rects[1],
        status: rects[2],
        composer: rects[3],
        footer: rects[4],
    }
}

fn layout_heights(total: u16, desired_composer: u16) -> [u16; 5] {
    match total {
        0 => [0, 0, 0, 0, 0],
        1 => [0, 0, 0, 1, 0],
        2 => [0, 1, 0, 1, 0],
        3 => [0, 1, 1, 1, 0],
        4 => [1, 1, 1, 1, 0],
        5 => [1, 1, 1, 1, 1],
        _ => {
            let header = if total >= 9 { 3 } else { 1 };
            let status = 1;
            let footer = 1;
            let reserved = header + status + footer + 1;
            let composer = desired_composer.min(total.saturating_sub(reserved));
            let transcript = total.saturating_sub(header + status + composer + footer);
            [header, transcript, status, composer, footer]
        }
    }
}

fn render_header(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let line = Line::from(vec![
        Span::styled(" afi ", Style::default().fg(Color::Cyan).bold()),
        Span::styled(&app.header, Style::default().fg(Color::White)),
    ]);
    if area.height >= 3 {
        frame.render_widget(
            Paragraph::new(line).block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
            area,
        );
    } else {
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn render_transcript(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let block = Block::bordered()
        .title(" Conversation ")
        .border_style(Style::default().fg(Color::DarkGray));
    let bordered = area.width >= 3 && area.height >= 3;
    let inner = if bordered {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        area
    };
    if inner.is_empty() {
        return;
    }
    let paragraph = Paragraph::new(transcript_text(&app.transcript)).wrap(Wrap { trim: false });
    let line_count = paragraph.line_count(inner.width);
    let last_page = line_count.saturating_sub(usize::from(inner.height));
    if app.scroll_from_bottom > 0 && app.rendered_transcript_revision != app.transcript_revision {
        let appended_lines = line_count.saturating_sub(app.rendered_transcript_lines);
        app.scroll_from_bottom = app.scroll_from_bottom.saturating_add(appended_lines);
    }
    app.rendered_transcript_revision = app.transcript_revision;
    app.rendered_transcript_lines = line_count;
    app.scroll_from_bottom = app.scroll_from_bottom.min(last_page);
    let top = last_page.saturating_sub(app.scroll_from_bottom);
    let scroll = u16::try_from(top).unwrap_or(u16::MAX);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
    if last_page > 0 && bordered {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_style(Color::DarkGray);
        let mut state = ScrollbarState::new(last_page.saturating_add(1))
            .position(top)
            .viewport_content_length(usize::from(inner.height));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

fn transcript_text(entries: &[TranscriptEntry]) -> Text<'_> {
    let mut lines = Vec::new();
    for entry in entries {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        append_entry_lines(&mut lines, entry);
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No messages yet.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    Text::from(lines)
}

fn append_entry_lines<'a>(lines: &mut Vec<Line<'a>>, entry: &'a TranscriptEntry) {
    let (label, style, markdown) = entry_format(entry.kind);
    let content = if markdown {
        tui_markdown::from_str(&entry.text)
    } else {
        Text::from(entry.text.as_str())
    };
    let content_lines = if content.lines.is_empty() {
        vec![Line::default()]
    } else {
        content.lines
    };
    for (index, line) in content_lines.into_iter().enumerate() {
        let prefix = if index == 0 {
            format!("{label:<11}")
        } else {
            " ".repeat(11)
        };
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(Span::styled(prefix, style));
        spans.extend(line.spans);
        lines.push(Line {
            style: line.style,
            alignment: line.alignment,
            spans,
        });
    }
}

fn entry_format(kind: EntryKind) -> (&'static str, Style, bool) {
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

fn render_status(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    if app.is_busy() {
        let label = app.activity.as_deref().unwrap_or("Working");
        let throbber = Throbber::default()
            .label(label)
            .style(Style::default().fg(Color::Cyan))
            .throbber_style(Style::default().fg(Color::Cyan).bold())
            .throbber_set(BRAILLE_SIX);
        frame.render_stateful_widget(throbber, area, &mut app.throbber);
    } else {
        frame.render_widget(
            Paragraph::new("Ready").style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }
}

fn render_footer(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.is_empty() {
        return;
    }
    let help = if app.approval.is_some() {
        "Y approve · N/Enter deny · Esc cancel · PgUp/PgDown or wheel inspect"
    } else if app.task_running {
        "Esc cancel · PgUp/PgDown or wheel scroll"
    } else {
        "Enter send · Alt+Enter newline · Alt+↑/↓ history · PgUp/PgDown or wheel scroll · Ctrl+C quit"
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_approval(frame: &mut Frame, app: &mut TuiApp) {
    let Some(prompt) = app.approval.as_deref() else {
        return;
    };
    let outer = frame.area();
    let width = outer.width.saturating_sub(2).clamp(1, 80);
    let text = Text::from(prompt);
    let measured = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(width.saturating_sub(2).max(1));
    let desired_height = u16::try_from(measured.saturating_add(2))
        .unwrap_or(u16::MAX)
        .max(5);
    let height = desired_height.min(outer.height.saturating_sub(2).max(1));
    let area = centered_rect(outer, width, height);
    if area.is_empty() {
        return;
    }
    frame.render_widget(Clear, area);
    let visible_height = usize::from(area.height.saturating_sub(2));
    let max_scroll = measured.saturating_sub(visible_height);
    app.approval_scroll = app.approval_scroll.min(max_scroll);
    let scroll = u16::try_from(app.approval_scroll).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::bordered()
                    .title(" Approval required ")
                    .title_bottom(" Y yes · N no · Esc cancel · PgUp/PgDown ")
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
        area,
    );
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width);
    let height = area.height.min(max_height);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}
