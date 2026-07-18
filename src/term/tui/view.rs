use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Shadow, Wrap,
};
use throbber_widgets_tui::{BRAILLE_SIX, Throbber};

use super::app::TuiApp;
use super::composer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutAreas {
    pub(super) header: Rect,
    pub(super) transcript: Rect,
    pub(super) status: Rect,
    pub(super) composer: Rect,
    pub(super) footer: Rect,
}

pub(super) fn render(frame: &mut Frame, app: &mut TuiApp) {
    let metrics = composer::measure_cached(&mut app.composer_view, &app.composer, frame.area());
    let areas = layout_areas(frame.area(), metrics.outer_height);
    render_header(frame, app, areas.header);
    render_transcript(frame, app, areas.transcript);
    render_status(frame, app, areas.status);
    composer::render(
        frame,
        &mut app.composer,
        &mut app.composer_view,
        areas.composer,
        metrics.visual_rows,
    );
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
        app.transcript_scroll_limit = Some(0);
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
        app.transcript_scroll_limit = Some(0);
        return;
    }
    let viewport = app.transcript_view.render(
        frame,
        &app.transcript,
        app.transcript_revision,
        inner,
        &mut app.scroll_from_bottom,
    );
    app.transcript_scroll_limit = Some(viewport.last_page);
    if viewport.last_page > 0 && bordered {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_style(Color::DarkGray);
        let mut state = ScrollbarState::new(viewport.last_page.saturating_add(1))
            .position(viewport.top)
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
        "Enter send · Alt+Enter newline · ↑/↓ edit/history · PgUp/PgDown or wheel scroll · Ctrl+C quit"
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
    let width = popup_extent(outer.width).min(80);
    let text = Text::from(prompt);
    let measured = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(width.saturating_sub(2).max(1));
    let desired_height = u16::try_from(measured.saturating_add(2))
        .unwrap_or(u16::MAX)
        .max(5);
    let height = desired_height.min(popup_extent(outer.height));
    let area = centered_rect(outer, width, height);
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().add_modifier(Modifier::DIM)),
        outer,
    );
    frame.render_widget(Clear, area);
    let visible_height = usize::from(area.height.saturating_sub(2));
    let max_scroll = measured.saturating_sub(visible_height);
    app.approval_scroll = app.approval_scroll.min(max_scroll);
    app.approval_scroll_limit = Some(max_scroll);
    let scroll = u16::try_from(app.approval_scroll).unwrap_or(u16::MAX);
    let surface = Style::default().fg(Color::White).bg(Color::Black);
    let accent = Style::default()
        .fg(Color::Yellow)
        .bg(Color::Black)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(text)
            .style(surface)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::bordered()
                    .border_type(BorderType::Thick)
                    .style(surface)
                    .border_style(accent)
                    .title_style(accent)
                    .title(" Approval required ")
                    .title_bottom(" Y yes · N no · Esc cancel · PgUp/PgDown ")
                    .shadow(
                        Shadow::dark_shade()
                            .style(Style::default().fg(Color::DarkGray).bg(Color::Black)),
                    ),
            ),
        area,
    );
    if max_scroll > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_style(accent);
        let mut state = ScrollbarState::new(max_scroll.saturating_add(1))
            .position(app.approval_scroll)
            .viewport_content_length(visible_height);
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

fn popup_extent(total: u16) -> u16 {
    if total >= 7 {
        total.saturating_sub(2)
    } else {
        total
    }
    .max(1)
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
