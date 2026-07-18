use std::mem;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::term::{MessageKind, StreamKind};

mod cache;
mod formatting;
mod streaming;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryKind {
    User,
    Message(MessageKind),
    Stream(StreamKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TranscriptEntry {
    pub(super) kind: EntryKind,
    pub(super) text: String,
    pub(super) revision: u64,
    pub(super) streaming: bool,
}

pub(super) struct ViewCache {
    entries: Vec<CachedEntry>,
    width: Option<u16>,
    total_lines: usize,
    synced_revision: Option<u64>,
    rendered_revision: u64,
    rendered_lines: usize,
    rendered_geometry: Option<(u16, u16)>,
    rendered_top: usize,
    rendered_scroll: usize,
    generation: u64,
    viewport_key: Option<ViewportKey>,
    viewport: Buffer,
    #[cfg(test)]
    stats: CacheStats,
}

struct CachedEntry {
    source_revision: u64,
    content: CachedContent,
    line_count: usize,
    start: usize,
}

enum CachedContent {
    Paragraph(Box<Paragraph<'static>>),
    Streaming(Box<streaming::StreamingText>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewportKey {
    generation: u64,
    width: u16,
    height: u16,
    top: usize,
}

#[derive(Debug, Clone, Copy)]
struct ScrollAnchor {
    entry: usize,
    local_row: usize,
    old_line_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Viewport {
    pub(super) last_page: usize,
    pub(super) top: usize,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CacheStats {
    pub(super) formatted_entries: usize,
    pub(super) markdown_parses: usize,
    pub(super) measured_entries: usize,
    pub(super) viewport_builds: usize,
    pub(super) content_checks: usize,
    pub(super) offset_updates: usize,
    /// Source bytes accepted from stream suffixes, not renderer work.
    pub(super) stream_input_bytes: usize,
}

impl Default for ViewCache {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            width: None,
            total_lines: 1,
            synced_revision: None,
            rendered_revision: 0,
            rendered_lines: 0,
            rendered_geometry: None,
            rendered_top: 0,
            rendered_scroll: 0,
            generation: 0,
            viewport_key: None,
            viewport: Buffer::empty(Rect::default()),
            #[cfg(test)]
            stats: CacheStats::default(),
        }
    }
}

impl ViewCache {
    pub(super) fn render(
        &mut self,
        frame: &mut Frame,
        source: &[TranscriptEntry],
        revision: u64,
        area: Rect,
        scroll_from_bottom: &mut usize,
    ) -> Viewport {
        let width_changed = self
            .rendered_geometry
            .is_some_and(|geometry| geometry.0 != area.width);
        let width_anchor = (self.rendered_scroll > 0 && width_changed)
            .then(|| self.scroll_anchor(self.rendered_top))
            .flatten();
        let requested_scroll = *scroll_from_bottom;
        self.sync(source, revision, area.width);
        let last_page = self.total_lines.saturating_sub(usize::from(area.height));
        let geometry = (area.width, area.height);
        if *scroll_from_bottom > 0
            && let Some(rendered_geometry) = self.rendered_geometry
        {
            if rendered_geometry != geometry {
                let top =
                    width_anchor.map_or(self.rendered_top, |anchor| self.resolve_anchor(anchor));
                let anchored = if self.rendered_scroll == 0 {
                    0
                } else {
                    last_page.saturating_sub(top.min(last_page))
                };
                *scroll_from_bottom =
                    adjusted_distance(anchored, requested_scroll, self.rendered_scroll);
            } else if self.rendered_revision != revision {
                let anchored = if self.rendered_scroll == 0 {
                    0
                } else {
                    shifted_distance(self.rendered_scroll, self.rendered_lines, self.total_lines)
                };
                *scroll_from_bottom =
                    adjusted_distance(anchored, requested_scroll, self.rendered_scroll);
            }
        }
        self.rendered_revision = revision;
        self.rendered_lines = self.total_lines;
        self.rendered_geometry = Some(geometry);
        *scroll_from_bottom = (*scroll_from_bottom).min(last_page);
        self.rendered_scroll = *scroll_from_bottom;
        let top = last_page.saturating_sub(*scroll_from_bottom);
        self.rendered_top = top;
        self.prepare_viewport(area.width, area.height, top);
        frame.render_widget(CachedBuffer(&self.viewport), area);
        Viewport { last_page, top }
    }

    fn scroll_anchor(&self, top: usize) -> Option<ScrollAnchor> {
        let mut entry = self.entries.partition_point(|cached| cached.start <= top);
        entry = entry.saturating_sub(1);
        let cached = self.entries.get(entry)?;
        let end = cached.start.saturating_add(cached.line_count);
        if top >= end && entry + 1 < self.entries.len() {
            entry += 1;
        }
        let cached = &self.entries[entry];
        Some(ScrollAnchor {
            entry,
            local_row: top.saturating_sub(cached.start).min(cached.line_count),
            old_line_count: cached.line_count,
        })
    }

    fn resolve_anchor(&self, anchor: ScrollAnchor) -> usize {
        let Some(entry) = self.entries.get(anchor.entry) else {
            return self.rendered_top;
        };
        let local = scaled_row(anchor.local_row, anchor.old_line_count, entry.line_count);
        entry.start.saturating_add(local)
    }

    fn prepare_viewport(&mut self, width: u16, height: u16, top: usize) {
        let key = ViewportKey {
            generation: self.generation,
            width,
            height,
            top,
        };
        if self.viewport_key == Some(key) {
            return;
        }
        self.viewport.resize(Rect::new(0, 0, width, height));
        self.viewport.reset();
        if self.entries.is_empty() {
            Paragraph::new(Line::styled(
                "No messages yet.",
                Style::default().fg(Color::DarkGray),
            ))
            .render(self.viewport.area, &mut self.viewport);
        } else {
            render_entries(&mut self.entries, &mut self.viewport, top);
        }
        self.viewport_key = Some(key);
        #[cfg(test)]
        {
            self.stats.viewport_builds += 1;
        }
    }

    #[cfg(test)]
    pub(super) const fn stats(&self) -> CacheStats {
        self.stats
    }
}

impl CachedContent {
    fn render(&mut self, area: Rect, viewport: &mut Buffer, top: usize) {
        match self {
            Self::Paragraph(paragraph) => {
                let scrolled = mem::take(paragraph.as_mut()).scroll((as_u16(top), 0));
                **paragraph = scrolled;
                (&**paragraph).render(area, viewport);
            }
            Self::Streaming(text) => text.render(area, viewport, top),
        }
    }
}

fn render_entries(entries: &mut [CachedEntry], viewport: &mut Buffer, top: usize) {
    let bottom = top.saturating_add(usize::from(viewport.area.height));
    let first =
        entries.partition_point(|entry| entry.start.saturating_add(entry.line_count) <= top);
    for entry in &mut entries[first..] {
        if entry.start >= bottom {
            break;
        }
        let entry_end = entry.start.saturating_add(entry.line_count);
        let visible_start = entry.start.max(top);
        let visible_end = entry_end.min(bottom);
        if visible_start >= visible_end {
            continue;
        }
        let local_top = visible_start.saturating_sub(entry.start);
        let screen_top = visible_start.saturating_sub(top);
        let visible_height = visible_end.saturating_sub(visible_start);
        let area = Rect::new(
            0,
            as_u16(screen_top),
            viewport.area.width,
            as_u16(visible_height),
        );
        entry.content.render(area, viewport, local_top);
    }
}

fn shifted_distance(distance: usize, old_lines: usize, new_lines: usize) -> usize {
    if new_lines >= old_lines {
        distance.saturating_add(new_lines - old_lines)
    } else {
        distance.saturating_sub(old_lines - new_lines)
    }
}

fn adjusted_distance(anchored: usize, requested: usize, rendered: usize) -> usize {
    if requested == 0 {
        return 0;
    }
    if requested >= rendered {
        anchored.saturating_add(requested - rendered)
    } else {
        anchored.saturating_sub(rendered - requested)
    }
}

fn scaled_row(local: usize, old_count: usize, new_count: usize) -> usize {
    if old_count == 0 || new_count == 0 {
        return 0;
    }
    local
        .saturating_mul(new_count)
        .checked_div(old_count)
        .unwrap_or_default()
        .min(new_count.saturating_sub(1))
}

fn as_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

struct CachedBuffer<'a>(&'a Buffer);

impl Widget for CachedBuffer<'_> {
    fn render(self, area: Rect, target: &mut Buffer) {
        let width = area.width.min(self.0.area.width);
        let height = area.height.min(self.0.area.height);
        for y in 0..height {
            for x in 0..width {
                target[(area.x + x, area.y + y)].clone_from(&self.0[(x, y)]);
            }
        }
    }
}
