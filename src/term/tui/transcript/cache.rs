use super::{CachedContent, CachedEntry, TranscriptEntry, ViewCache, formatting, streaming};

#[derive(Debug, Clone, Copy, Default)]
struct ContentSync {
    rebuilt_from: Option<usize>,
    offsets_from: Option<usize>,
}

impl ViewCache {
    pub(super) fn sync(&mut self, source: &[TranscriptEntry], revision: u64, width: u16) {
        let width_changed = self.width != Some(width);
        let content_changed =
            self.synced_revision != Some(revision) || self.entries.len() != source.len();
        let synced = if content_changed {
            self.sync_content(source, width)
        } else {
            ContentSync::default()
        };
        self.measure_entries(source, width, width_changed, synced.rebuilt_from);
        let offsets_from = if width_changed {
            Some(0)
        } else {
            synced.offsets_from
        };
        if let Some(start) = offsets_from {
            self.width = Some(width);
            self.rebuild_offsets(start);
            self.generation = self.generation.wrapping_add(1);
            self.viewport_key = None;
        }
        self.synced_revision = Some(revision);
    }

    fn sync_content(&mut self, source: &[TranscriptEntry], width: u16) -> ContentSync {
        let cached_len = self.entries.len();
        let length_changed = cached_len != source.len();
        let start = dirty_start(&self.entries, source);
        self.entries.truncate(source.len());
        let mut rebuilt_from = None;
        for (index, entry) in source.iter().enumerate().skip(start) {
            #[cfg(test)]
            {
                self.stats.content_checks += 1;
            }
            let unchanged = self
                .entries
                .get(index)
                .is_some_and(|cached| cached.source_revision == entry.revision);
            if unchanged {
                continue;
            }
            if entry.streaming
                && let Some(cached) = self.entries.get_mut(index)
                && let CachedContent::Streaming(streaming) = &mut cached.content
            {
                let processed = streaming.update(entry, width);
                cached.source_revision = entry.revision;
                #[cfg(test)]
                self.record_stream_update(processed);
                #[cfg(not(test))]
                let _ = processed;
                rebuilt_from.get_or_insert(index);
                continue;
            }
            let (content, markdown, streamed_bytes) = build_content(entry, width);
            #[cfg(test)]
            self.record_content_update(markdown, streamed_bytes);
            #[cfg(not(test))]
            let _ = (markdown, streamed_bytes);
            let cached = CachedEntry {
                source_revision: entry.revision,
                content,
                line_count: 0,
                start: 0,
            };
            if let Some(slot) = self.entries.get_mut(index) {
                *slot = cached;
            } else {
                self.entries.push(cached);
            }
            rebuilt_from.get_or_insert(index);
        }
        let offsets_from = match (length_changed, rebuilt_from) {
            (true, Some(rebuilt)) => Some(cached_len.min(source.len()).min(rebuilt)),
            (true, None) => Some(cached_len.min(source.len())),
            (false, rebuilt) => rebuilt,
        };
        ContentSync {
            rebuilt_from,
            offsets_from,
        }
    }

    fn measure_entries(
        &mut self,
        source: &[TranscriptEntry],
        width: u16,
        width_changed: bool,
        rebuilt_from: Option<usize>,
    ) {
        if width_changed {
            for (entry, source) in self.entries.iter_mut().zip(source) {
                entry.content.resize(source, width);
                entry.line_count = entry.content.line_count(width);
            }
            #[cfg(test)]
            self.record_measurements(self.entries.len());
        } else if let Some(start) = rebuilt_from {
            for entry in &mut self.entries[start..] {
                entry.line_count = entry.content.line_count(width);
            }
            #[cfg(test)]
            self.record_measurements(self.entries.len().saturating_sub(start));
        }
    }

    fn rebuild_offsets(&mut self, start: usize) {
        let count = self.entries.len();
        let start = start.min(count);
        let mut next = if start == 0 {
            0
        } else {
            let previous = &self.entries[start - 1];
            previous
                .start
                .saturating_add(previous.line_count)
                .saturating_add(1)
        };
        for (index, entry) in self.entries.iter_mut().enumerate().skip(start) {
            entry.start = next;
            next = next.saturating_add(entry.line_count);
            if index + 1 < count {
                next = next.saturating_add(1);
            }
        }
        self.total_lines = self
            .entries
            .last()
            .map_or(1, |entry| entry.start.saturating_add(entry.line_count));
        #[cfg(test)]
        {
            self.stats.offset_updates += count.saturating_sub(start);
        }
    }

    #[cfg(test)]
    fn record_measurements(&mut self, count: usize) {
        self.stats.measured_entries += count;
    }

    #[cfg(test)]
    fn record_content_update(&mut self, markdown: bool, streamed_bytes: usize) {
        self.stats.formatted_entries += 1;
        self.stats.markdown_parses += usize::from(markdown);
        self.stats.stream_input_bytes += streamed_bytes;
    }

    #[cfg(test)]
    fn record_stream_update(&mut self, streamed_bytes: usize) {
        self.stats.formatted_entries += 1;
        self.stats.stream_input_bytes += streamed_bytes;
    }
}

impl CachedContent {
    fn resize(&mut self, source: &TranscriptEntry, width: u16) {
        match self {
            Self::Paragraph(_) => {}
            Self::Streaming(text) => {
                let _ = text.update(source, width);
            }
        }
    }

    fn line_count(&self, width: u16) -> usize {
        match self {
            Self::Paragraph(paragraph) => paragraph.line_count(width),
            Self::Streaming(text) => text.line_count(),
        }
    }
}

fn build_content(entry: &TranscriptEntry, width: u16) -> (CachedContent, bool, usize) {
    if entry.streaming {
        return (
            CachedContent::Streaming(Box::new(streaming::StreamingText::new(entry, width))),
            false,
            entry.text.len(),
        );
    }
    let (paragraph, markdown) = formatting::entry(entry);
    (CachedContent::Paragraph(Box::new(paragraph)), markdown, 0)
}

fn dirty_start(cached: &[CachedEntry], source: &[TranscriptEntry]) -> usize {
    if cached.is_empty() || source.len() < cached.len() {
        return source.len().min(cached.len());
    }
    if source.len() > cached.len() {
        let tail = cached.len() - 1;
        return if cached[tail].source_revision == source[tail].revision {
            cached.len()
        } else {
            tail
        };
    }
    cached.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use std::slice;

    use crate::term::StreamKind;

    use super::super::EntryKind;
    use super::*;

    #[test]
    fn resize_with_append_counts_only_the_new_stream_input() {
        let mut cache = ViewCache::default();
        let mut entry = TranscriptEntry {
            kind: EntryKind::Stream(StreamKind::Assistant),
            text: "abc".to_string(),
            revision: 1,
            streaming: true,
        };
        cache.sync(slice::from_ref(&entry), 1, 60);
        assert_eq!(cache.stats().stream_input_bytes, 3);

        entry.text.push('d');
        entry.revision += 1;
        cache.sync(slice::from_ref(&entry), 2, 72);
        assert_eq!(cache.stats().stream_input_bytes, 4);
    }
}
