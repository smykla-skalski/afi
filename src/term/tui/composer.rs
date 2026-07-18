use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Clear, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};

const MAX_CONTENT_ROWS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Metrics {
    pub(super) outer_height: u16,
    pub(super) visual_rows: usize,
}

#[derive(Debug, Default)]
pub(super) struct ViewState {
    scroll_top: usize,
    viewport_height: u16,
    draft_scroll_top: Option<usize>,
    layout_revision: u64,
    metrics: Option<CachedMetrics>,
    #[cfg(test)]
    measurement_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct CachedMetrics {
    revision: u64,
    width: u16,
    metrics: Metrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InputView {
    layout_revision: u64,
    cursor: (usize, usize),
    selection: Option<((usize, usize), (usize, usize))>,
    scroll_top: usize,
}

impl ViewState {
    pub(super) fn key_scroll_delta(&self, key: KeyEvent) -> i16 {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = i16::try_from(self.viewport_height).unwrap_or(i16::MAX);
        match key.code {
            KeyCode::Char('v') if ctrl && !alt => page,
            KeyCode::Char('v') if alt && !ctrl => -page,
            _ => 0,
        }
    }

    pub(super) fn record_scroll(&mut self, rows: i16) {
        self.scroll_top = apply_scroll(self.scroll_top, rows);
    }

    pub(super) fn save_draft(&mut self) {
        self.draft_scroll_top = Some(self.scroll_top);
    }

    pub(super) fn restore_draft(&mut self) {
        if let Some(top) = self.draft_scroll_top.take() {
            self.scroll_top = top;
            self.invalidate_layout();
        }
    }

    pub(super) fn clear_draft(&mut self) {
        self.draft_scroll_top = None;
    }

    pub(super) fn invalidate_layout(&mut self) {
        self.layout_revision = self.layout_revision.wrapping_add(1);
        self.metrics = None;
    }

    pub(super) const fn layout_revision(&self) -> u64 {
        self.layout_revision
    }

    pub(super) const fn scroll_top(&self) -> usize {
        self.scroll_top
    }

    #[cfg(test)]
    pub(super) const fn measurement_count(&self) -> usize {
        self.measurement_count
    }
}

pub(super) fn input_view(textarea: &TextArea<'_>, state: &ViewState) -> InputView {
    let cursor = textarea.cursor();
    InputView {
        layout_revision: state.layout_revision(),
        cursor: (cursor.0, cursor.1),
        selection: textarea.selection_range(),
        scroll_top: state.scroll_top(),
    }
}

pub(super) fn new() -> TextArea<'static> {
    let mut composer = TextArea::default();
    composer.set_block(
        Block::bordered()
            .title(" Message ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    composer.set_placeholder_text("Ask afi to inspect or change something…");
    composer.set_placeholder_style(Style::default().fg(Color::DarkGray));
    composer.set_cursor_line_style(Style::default());
    composer.set_cursor_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    composer.set_wrap_mode(WrapMode::WordOrGlyph);
    composer
}

pub(super) fn normalize_newlines(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                let _ = chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    Cow::Owned(normalized)
}

pub(super) fn measure(textarea: &TextArea<'_>, frame_area: Rect) -> Metrics {
    let has_border = textarea.block().is_some() && frame_area.width >= 3;
    let border_rows = usize::from(has_border) * 2;
    let inner_width = if has_border {
        frame_area.width.saturating_sub(2)
    } else {
        frame_area.width
    }
    .max(1);
    let visual_rows = visual_rows(textarea, inner_width);
    let visible_rows = visual_rows.clamp(1, MAX_CONTENT_ROWS);
    Metrics {
        outer_height: u16::try_from(visible_rows + border_rows).unwrap_or(u16::MAX),
        visual_rows,
    }
}

pub(super) fn measure_cached(
    state: &mut ViewState,
    textarea: &TextArea<'_>,
    frame_area: Rect,
) -> Metrics {
    if let Some(cached) = state.metrics
        && cached.revision == state.layout_revision
        && cached.width == frame_area.width
    {
        return cached.metrics;
    }
    let metrics = measure(textarea, frame_area);
    state.metrics = Some(CachedMetrics {
        revision: state.layout_revision,
        width: frame_area.width,
        metrics,
    });
    #[cfg(test)]
    {
        state.measurement_count += 1;
    }
    metrics
}

pub(super) fn render(
    frame: &mut Frame,
    textarea: &mut TextArea<'static>,
    state: &mut ViewState,
    area: Rect,
    visual_rows: usize,
) {
    if area.is_empty() {
        state.viewport_height = 0;
        return;
    }
    let block = textarea.block().cloned();
    let bordered = block.is_some() && area.width >= 3 && area.height >= 3;
    let inner = if bordered {
        block.as_ref().expect("border checked above").inner(area)
    } else {
        area
    };
    let visual_rows = if bordered {
        visual_rows
    } else {
        self::visual_rows(textarea, inner.width.max(1))
    };
    render_textarea(frame, textarea, area, bordered);
    if sync_scroll(textarea, state, inner, visual_rows) {
        frame.render_widget(Clear, area);
        render_textarea(frame, textarea, area, bordered);
    }
    render_scrollbar(frame, state, area, inner, visual_rows, bordered);
}

fn visual_rows(textarea: &TextArea<'_>, width: u16) -> usize {
    let mut probe = TextArea::new(textarea.lines().to_vec());
    probe.set_tab_length(textarea.tab_length());
    probe.set_wrap_mode(textarea.wrap_mode());
    if let Some(style) = textarea.line_number_style() {
        probe.set_line_number_style(style);
    }
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    (&probe).render(area, &mut buffer);
    probe.move_cursor(CursorMove::Bottom);
    probe.move_cursor(CursorMove::End);
    probe.screen_cursor().row.saturating_add(1)
}

fn render_textarea(
    frame: &mut Frame,
    textarea: &mut TextArea<'static>,
    area: Rect,
    bordered: bool,
) {
    if bordered {
        frame.render_widget(&*textarea, area);
        return;
    }
    let block = textarea.block().cloned();
    textarea.remove_block();
    frame.render_widget(&*textarea, area);
    if let Some(block) = block {
        textarea.set_block(block);
    }
}

fn sync_scroll(
    textarea: &mut TextArea<'static>,
    state: &mut ViewState,
    inner: Rect,
    visual_rows: usize,
) -> bool {
    state.viewport_height = inner.height;
    if textarea.is_empty() {
        state.scroll_top = 0;
        return false;
    }
    let cursor = textarea.screen_cursor().row;
    let height = usize::from(inner.height);
    let top = if cursor < state.scroll_top {
        cursor
    } else if state.scroll_top.saturating_add(height) <= cursor {
        cursor.saturating_add(1).saturating_sub(height)
    } else {
        state.scroll_top
    };
    let last_page = visual_rows.saturating_sub(height);
    let clamped = top.min(last_page);
    state.scroll_top = top;
    if clamped == top {
        return false;
    }
    set_scroll_top(textarea, state, clamped);
    true
}

fn render_scrollbar(
    frame: &mut Frame,
    state: &ViewState,
    area: Rect,
    inner: Rect,
    visual_rows: usize,
    bordered: bool,
) {
    let viewport = usize::from(inner.height);
    let last_page = visual_rows.saturating_sub(viewport);
    if !bordered || last_page == 0 {
        return;
    }
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(None)
        .thumb_style(Color::DarkGray);
    let mut scrollbar_state = ScrollbarState::new(last_page.saturating_add(1))
        .position(state.scroll_top.min(last_page))
        .viewport_content_length(viewport);
    frame.render_stateful_widget(
        scrollbar,
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut scrollbar_state,
    );
}

fn apply_scroll(position: usize, rows: i16) -> usize {
    match usize::try_from(rows) {
        Ok(rows) => position.saturating_add(rows),
        Err(_) => position.saturating_sub(usize::from(rows.unsigned_abs())),
    }
}

fn set_scroll_top(textarea: &mut TextArea<'static>, state: &mut ViewState, target: usize) {
    let max_step = usize::try_from(i16::MAX).expect("i16::MAX fits usize");
    while state.scroll_top < target {
        let remaining = target - state.scroll_top;
        let step = i16::try_from(remaining.min(max_step)).expect("scroll step is clamped");
        textarea.scroll((step, 0));
        state.scroll_top += usize::try_from(step).expect("scroll step is positive");
    }
    while state.scroll_top > target {
        let remaining = state.scroll_top - target;
        let step = i16::try_from(remaining.min(max_step)).expect("scroll step is clamped");
        textarea.scroll((-step, 0));
        state.scroll_top -= usize::try_from(step).expect("scroll step is positive");
    }
}
