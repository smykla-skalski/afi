use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui_textarea::{CursorMove, TextArea};
use throbber_widgets_tui::ThrobberState;

use crate::risk::ApprovalChoice;

use super::transcript::{self, EntryKind, TranscriptEntry};
use super::{composer, view};

mod output;

const SCROLL_STEP: usize = 5;

/// Result of applying one terminal key to the fullscreen UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    None,
    Submit(String),
    Quit,
    CancelTask,
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
    pub(super) approval_scroll_limit: Option<usize>,
    approval_choice: Option<ApprovalChoice>,
    active_stream: Option<usize>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<TextArea<'static>>,
    pub(super) scroll_from_bottom: usize,
    pub(super) transcript_scroll_limit: Option<usize>,
    pub(super) transcript_revision: u64,
    pub(super) transcript_view: transcript::ViewCache,
}

impl TuiApp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            header: String::new(),
            transcript: Vec::new(),
            composer: composer::new(),
            composer_view: composer::ViewState::default(),
            activity: None,
            task_running: false,
            throbber: ThrobberState::default(),
            approval: None,
            approval_scroll: 0,
            approval_scroll_limit: None,
            approval_choice: None,
            active_stream: None,
            history: Vec::new(),
            history_index: None,
            history_draft: None,
            scroll_from_bottom: 0,
            transcript_scroll_limit: None,
            transcript_revision: 0,
            transcript_view: transcript::ViewCache::default(),
        }
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.task_running = running;
    }

    pub fn set_activity(&mut self, activity: Option<String>) {
        let _ = self.set_activity_with_redraw(activity);
    }

    pub(crate) fn set_activity_with_redraw(&mut self, activity: Option<String>) -> bool {
        let changed = self.activity != activity;
        self.activity = activity;
        changed
    }

    pub fn set_approval(&mut self, approval: Option<String>) {
        self.approval = approval;
        self.approval_scroll = 0;
        self.approval_scroll_limit = None;
        self.approval_choice = None;
    }

    /// Return the last modal decision, once.
    pub fn take_approval_choice(&mut self) -> Option<ApprovalChoice> {
        self.approval_choice.take()
    }

    pub fn tick(&mut self) {
        if self.should_animate() {
            self.throbber.calc_next();
        }
    }

    pub(crate) fn handle_key_with_redraw(&mut self, key: KeyEvent) -> (InputAction, bool) {
        let before = self.input_view();
        let action = self.handle_key(key);
        (action, before != self.input_view())
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
        let _ = self.handle_mouse_with_redraw(mouse);
    }

    pub(crate) fn handle_mouse_with_redraw(&mut self, mouse: MouseEvent) -> bool {
        let before = (self.scroll_from_bottom, self.approval_scroll);
        match (self.approval.is_some(), mouse.kind) {
            (true, MouseEventKind::ScrollUp) => {
                self.approval_scroll = self.approval_scroll.saturating_sub(SCROLL_STEP);
            }
            (true, MouseEventKind::ScrollDown) => {
                self.scroll_approval_down();
            }
            (false, MouseEventKind::ScrollUp) => self.scroll_up(),
            (false, MouseEventKind::ScrollDown) => self.scroll_down(),
            _ => {}
        }
        before != (self.scroll_from_bottom, self.approval_scroll)
    }

    pub fn paste(&mut self, text: &str) {
        let _ = self.paste_with_redraw(text);
    }

    pub(crate) fn paste_with_redraw(&mut self, text: &str) -> bool {
        if self.approval.is_some() || self.task_running {
            return false;
        }
        let before = self.input_text();
        let was_selecting = self.composer.is_selecting();
        let text = composer::normalize_newlines(text);
        let _ = self.composer.insert_str(text.as_ref());
        let changed = self.input_text() != before || self.composer.is_selecting() != was_selecting;
        if changed {
            self.composer_view.invalidate_layout();
            self.reset_history_navigation();
        }
        changed
    }

    pub fn scroll_up(&mut self) {
        let next = self.scroll_from_bottom.saturating_add(SCROLL_STEP);
        self.scroll_from_bottom = self
            .transcript_scroll_limit
            .map_or(next, |limit| next.min(limit));
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

    pub(crate) fn should_animate(&self) -> bool {
        self.is_busy() && self.approval.is_none()
    }

    fn input_view(&self) -> (composer::InputView, u64, usize, usize, bool) {
        (
            composer::input_view(&self.composer, &self.composer_view),
            self.transcript_revision,
            self.scroll_from_bottom,
            self.approval_scroll,
            self.approval.is_some(),
        )
    }

    fn handle_approval_key(&mut self, key: KeyEvent) -> InputAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::PageUp | KeyCode::Up => {
                self.approval_scroll = self.approval_scroll.saturating_sub(SCROLL_STEP);
                return InputAction::None;
            }
            KeyCode::PageDown | KeyCode::Down => {
                self.scroll_approval_down();
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

    fn scroll_approval_down(&mut self) {
        let next = self.approval_scroll.saturating_add(SCROLL_STEP);
        self.approval_scroll = self
            .approval_scroll_limit
            .map_or(next, |limit| next.min(limit));
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
            self.composer_view.invalidate_layout();
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
        self.composer_view.invalidate_layout();
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
        self.composer_view.invalidate_layout();
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
