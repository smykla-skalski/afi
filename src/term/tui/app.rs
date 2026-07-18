use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Block;
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use throbber_widgets_tui::ThrobberState;

use crate::risk::ApprovalChoice;
use crate::term::{MessageKind, OutputEvent, StreamKind};

use super::{composer, view};

const SCROLL_STEP: usize = 5;

/// Result of applying one terminal key to the fullscreen UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    None,
    Submit(String),
    Quit,
    CancelTask,
}

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
}

/// Stateful, terminal-independent REPL presentation model.
pub struct TuiApp {
    pub(super) header: String,
    pub(super) transcript: Vec<TranscriptEntry>,
    pub(super) composer: TextArea<'static>,
    pub(super) composer_view: composer::ViewState,
    pub(super) activity: Option<String>,
    pub(super) task_running: bool,
    pub(super) throbber: ThrobberState,
    pub(super) approval: Option<String>,
    pub(super) approval_scroll: usize,
    approval_choice: Option<ApprovalChoice>,
    active_stream: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<TextArea<'static>>,
    pub(super) scroll_from_bottom: usize,
    pub(super) transcript_revision: u64,
    pub(super) rendered_transcript_revision: u64,
    pub(super) rendered_transcript_lines: usize,
}

impl TuiApp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            header: String::new(),
            transcript: Vec::new(),
            composer: make_composer(Vec::new()),
            composer_view: composer::ViewState::default(),
            activity: None,
            task_running: false,
            throbber: ThrobberState::default(),
            approval: None,
            approval_scroll: 0,
            approval_choice: None,
            active_stream: None,
            history: Vec::new(),
            history_index: None,
            history_draft: None,
            scroll_from_bottom: 0,
            transcript_revision: 0,
            rendered_transcript_revision: 0,
            rendered_transcript_lines: 0,
        }
    }

    pub fn apply_output(&mut self, event: OutputEvent) {
        match event {
            OutputEvent::Header(text) => self.header = text,
            OutputEvent::Message { kind, text } => {
                self.push_entry(EntryKind::Message(kind), text);
            }
            OutputEvent::Stream { kind, delta } => self.append_stream(kind, &delta),
            OutputEvent::StreamFinished => self.active_stream = None,
            OutputEvent::ToolStarted { name, action } => {
                self.push_entry(
                    EntryKind::Message(MessageKind::Tool),
                    format!("{name}: {action}"),
                );
            }
            OutputEvent::ToolFinished { name, summary } => {
                self.push_entry(
                    EntryKind::Message(MessageKind::Tool),
                    format!("{name}: {summary}"),
                );
            }
        }
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.task_running = running;
    }

    pub fn set_activity(&mut self, activity: Option<String>) {
        self.activity = activity;
    }

    pub fn set_approval(&mut self, approval: Option<String>) {
        self.approval = approval;
        self.approval_scroll = 0;
        self.approval_choice = None;
    }

    /// Return the last modal decision, once.
    pub fn take_approval_choice(&mut self) -> Option<ApprovalChoice> {
        self.approval_choice.take()
    }

    pub fn tick(&mut self) {
        if self.is_busy() {
            self.throbber.calc_next();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        if key.kind == KeyEventKind::Release {
            return InputAction::None;
        }
        if self.approval.is_some() {
            return self.handle_approval_key(key);
        }
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            return self.handle_scroll_key(key.code);
        }
        if self.task_running {
            return match key.code {
                KeyCode::Esc => InputAction::CancelTask,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    InputAction::CancelTask
                }
                _ => InputAction::None,
            };
        }
        self.handle_composer_key(key)
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match (self.approval.is_some(), mouse.kind) {
            (true, MouseEventKind::ScrollUp) => {
                self.approval_scroll = self.approval_scroll.saturating_sub(SCROLL_STEP);
            }
            (true, MouseEventKind::ScrollDown) => {
                self.approval_scroll = self.approval_scroll.saturating_add(SCROLL_STEP);
            }
            (false, MouseEventKind::ScrollUp) => self.scroll_up(),
            (false, MouseEventKind::ScrollDown) => self.scroll_down(),
            _ => {}
        }
    }

    pub fn paste(&mut self, text: &str) {
        if self.approval.is_some() || self.task_running {
            return;
        }
        let before = self.input_text();
        let was_selecting = self.composer.is_selecting();
        let text = composer::normalize_newlines(text);
        let _ = self.composer.insert_str(text.as_ref());
        if self.input_text() != before || self.composer.is_selecting() != was_selecting {
            self.reset_history_navigation();
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(SCROLL_STEP);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(SCROLL_STEP);
    }

    pub fn render(&mut self, frame: &mut Frame) {
        view::render(frame, self);
    }

    pub(super) fn input_text(&self) -> String {
        self.composer.lines().join("\n")
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.task_running || self.activity.is_some()
    }

    fn append_stream(&mut self, kind: StreamKind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(index) = self.active_stream
            && self.transcript[index].kind == EntryKind::Stream(kind)
        {
            self.transcript[index].text.push_str(delta);
            self.transcript_revision = self.transcript_revision.wrapping_add(1);
            return;
        }
        self.transcript.push(TranscriptEntry {
            kind: EntryKind::Stream(kind),
            text: delta.to_string(),
        });
        self.active_stream = Some(self.transcript.len() - 1);
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
    }

    fn push_entry(&mut self, kind: EntryKind, text: String) {
        self.active_stream = None;
        if !text.is_empty() {
            self.transcript.push(TranscriptEntry { kind, text });
            self.transcript_revision = self.transcript_revision.wrapping_add(1);
        }
    }

    fn handle_approval_key(&mut self, key: KeyEvent) -> InputAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::PageUp | KeyCode::Up => {
                self.approval_scroll = self.approval_scroll.saturating_sub(SCROLL_STEP);
                return InputAction::None;
            }
            KeyCode::PageDown | KeyCode::Down => {
                self.approval_scroll = self.approval_scroll.saturating_add(SCROLL_STEP);
                return InputAction::None;
            }
            _ => {}
        }
        let choice = match key.code {
            KeyCode::Char('y' | 'Y') => Some(ApprovalChoice::Yes),
            KeyCode::Char('n' | 'N') | KeyCode::Enter => Some(ApprovalChoice::No),
            KeyCode::Esc => Some(ApprovalChoice::Esc),
            KeyCode::Char('c') if ctrl => Some(ApprovalChoice::Esc),
            _ => None,
        };
        if let Some(choice) = choice {
            self.approval = None;
            self.approval_choice = Some(choice);
        }
        InputAction::None
    }

    fn handle_scroll_key(&mut self, code: KeyCode) -> InputAction {
        if code == KeyCode::PageUp {
            self.scroll_up();
        } else {
            self.scroll_down();
        }
        InputAction::None
    }

    fn handle_composer_key(&mut self, key: KeyEvent) -> InputAction {
        if key.modifiers.is_empty() && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            self.move_or_navigate_history(key);
            return InputAction::None;
        }
        self.handle_composer_edit_key(key)
    }

    fn handle_composer_edit_key(&mut self, key: KeyEvent) -> InputAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let history_alt = key.modifiers == KeyModifiers::ALT;
        match key.code {
            KeyCode::Enter if alt => self.insert_newline(),
            KeyCode::Char('j') if ctrl => self.insert_newline(),
            KeyCode::Enter => return self.submit(),
            KeyCode::Up if history_alt => self.history_previous(),
            KeyCode::Down if history_alt => self.history_next(),
            KeyCode::Char('c') if ctrl => return InputAction::Quit,
            KeyCode::Char('d') if ctrl && self.composer.is_empty() => return InputAction::Quit,
            _ => self.apply_composer_input(key),
        }
        InputAction::None
    }

    fn apply_composer_input(&mut self, key: KeyEvent) {
        let scroll = self.composer_view.key_scroll_delta(key);
        if self.composer.input(key) {
            self.reset_history_navigation();
        }
        self.composer_view.record_scroll(scroll);
    }

    fn move_or_navigate_history(&mut self, key: KeyEvent) {
        let row = self.composer.screen_cursor().row;
        let _ = self.composer.input(key);
        if self.composer.screen_cursor().row != row || self.composer.is_selecting() {
            return;
        }
        match key.code {
            KeyCode::Up => self.history_previous(),
            KeyCode::Down if self.history_index.is_some() => self.history_next(),
            _ => {}
        }
    }

    fn insert_newline(&mut self) {
        self.composer.insert_newline();
        self.reset_history_navigation();
    }

    fn submit(&mut self) -> InputAction {
        let text = self.input_text();
        if text.trim().is_empty() {
            return InputAction::None;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.push_entry(EntryKind::User, text.clone());
        self.set_composer_text("");
        self.reset_history_navigation();
        self.scroll_from_bottom = 0;
        InputAction::Submit(text)
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(0) => return,
            Some(index) => index - 1,
            None => {
                self.history_draft = Some(self.composer.clone());
                self.composer_view.save_draft();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        let text = self.history[index].clone();
        self.set_composer_text(&text);
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            let text = self.history[index + 1].clone();
            self.set_composer_text(&text);
        } else {
            self.history_index = None;
            if let Some(draft) = self.history_draft.take() {
                self.composer = draft;
                self.composer_view.restore_draft();
            }
        }
    }

    fn set_composer_text(&mut self, text: &str) {
        let max_histories = self.composer.max_histories();
        self.composer.cancel_selection();
        self.composer.clear();
        let _ = self.composer.insert_str(text);
        self.composer.set_max_histories(max_histories);
        self.composer.move_cursor(CursorMove::Bottom);
        self.composer.move_cursor(CursorMove::End);
    }

    fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.history_draft = None;
        self.composer_view.clear_draft();
    }
}

impl Default for TuiApp {
    fn default() -> Self {
        Self::new()
    }
}

fn make_composer(lines: Vec<String>) -> TextArea<'static> {
    let mut composer = TextArea::new(lines);
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
