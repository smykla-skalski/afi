use crate::term::{MessageKind, OutputEvent, StreamKind};

use super::super::transcript::{EntryKind, TranscriptEntry};
use super::TuiApp;

impl TuiApp {
    pub fn apply_output(&mut self, event: OutputEvent) {
        let _ = self.apply_output_with_redraw(event);
    }

    pub(crate) fn apply_output_with_redraw(&mut self, event: OutputEvent) -> bool {
        match event {
            OutputEvent::Header(text) => self.set_header(text),
            OutputEvent::Message { kind, text } => self.push_entry(EntryKind::Message(kind), text),
            OutputEvent::Stream { kind, delta } => self.append_stream(kind, &delta),
            OutputEvent::StreamFinished => self.finish_stream(),
            OutputEvent::ToolStarted { name, action } => self.push_entry(
                EntryKind::Message(MessageKind::Tool),
                format!("{name}: {action}"),
            ),
            OutputEvent::ToolFinished { name, summary } => self.push_entry(
                EntryKind::Message(MessageKind::Tool),
                format!("{name}: {summary}"),
            ),
        }
    }

    fn set_header(&mut self, text: String) -> bool {
        if self.header == text {
            return false;
        }
        self.header = text;
        true
    }

    fn append_stream(&mut self, kind: StreamKind, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        if let Some(index) = self.active_stream
            && self.transcript[index].kind == EntryKind::Stream(kind)
        {
            self.transcript[index].text.push_str(delta);
            self.transcript[index].streaming = true;
            let revision = self.advance_transcript_revision();
            self.transcript[index].revision = revision;
            return true;
        }
        let _ = self.finish_stream();
        let revision = self.advance_transcript_revision();
        self.transcript.push(TranscriptEntry {
            kind: EntryKind::Stream(kind),
            text: delta.to_string(),
            revision,
            streaming: true,
        });
        self.active_stream = Some(self.transcript.len() - 1);
        true
    }

    pub(super) fn push_entry(&mut self, kind: EntryKind, text: String) -> bool {
        let finalized = self.finish_stream();
        if text.is_empty() {
            return finalized;
        }
        let revision = self.advance_transcript_revision();
        self.transcript.push(TranscriptEntry {
            kind,
            text,
            revision,
            streaming: false,
        });
        true
    }

    fn finish_stream(&mut self) -> bool {
        let Some(index) = self.active_stream.take() else {
            return false;
        };
        if !self.transcript[index].streaming {
            return false;
        }
        let revision = self.advance_transcript_revision();
        self.transcript[index].revision = revision;
        self.transcript[index].streaming = false;
        true
    }

    fn advance_transcript_revision(&mut self) -> u64 {
        self.transcript_scroll_limit = None;
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
        self.transcript_revision
    }
}
