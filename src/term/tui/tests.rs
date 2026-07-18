use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::risk::ApprovalChoice;
use crate::term::{MessageKind, OutputEvent, StreamKind};

use super::app::{InputAction, TuiApp};
use super::view::layout_areas;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn modified(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn render(app: &mut TuiApp, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn row(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

fn marker_row(buffer: &Buffer, marker: &str) -> u16 {
    (0..buffer.area.height)
        .find(|&y| row(buffer, y).contains(marker))
        .unwrap_or_else(|| panic!("missing marker {marker:?}"))
}

#[test]
fn stream_deltas_merge_until_finished() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "Hello ".to_string(),
    });
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "**world**".to_string(),
    });
    assert_eq!(app.transcript.len(), 1);
    assert_eq!(app.transcript[0].text, "Hello **world**");

    app.apply_output(OutputEvent::StreamFinished);
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "Again".to_string(),
    });
    assert_eq!(app.transcript.len(), 2);
}

#[test]
fn composer_submit_and_multiline_bindings() {
    let mut app = TuiApp::new();
    app.paste("first");
    assert_eq!(
        app.handle_key(modified(KeyCode::Enter, KeyModifiers::ALT)),
        InputAction::None
    );
    app.paste("second");
    assert_eq!(
        app.handle_key(modified(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        InputAction::None
    );
    app.paste("third");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        InputAction::Submit("first\nsecond\nthird".to_string())
    );
    assert!(app.composer.is_empty());
    assert_eq!(app.transcript.len(), 1);
}

#[test]
fn history_navigation_restores_draft() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("draft");

    let _ = app.handle_key(modified(KeyCode::Up, KeyModifiers::ALT));
    assert_eq!(app.input_text(), "prior");
    let _ = app.handle_key(modified(KeyCode::Down, KeyModifiers::ALT));
    assert_eq!(app.input_text(), "draft");
}

#[test]
fn running_task_owns_escape_and_blocks_composer() {
    let mut app = TuiApp::new();
    app.set_task_running(true);
    app.paste("ignored");
    assert!(app.composer.is_empty());
    assert_eq!(app.handle_key(key(KeyCode::Esc)), InputAction::CancelTask);
    assert_eq!(app.handle_key(key(KeyCode::Char('x'))), InputAction::None);
    assert!(app.composer.is_empty());
}

#[test]
fn modal_keys_do_not_leak_into_composer() {
    let mut app = TuiApp::new();
    app.paste("draft");
    app.set_approval(Some("run command?".to_string()));
    assert_eq!(app.handle_key(key(KeyCode::Char('x'))), InputAction::None);
    assert_eq!(app.input_text(), "draft");
    assert_eq!(app.take_approval_choice(), None);
}

#[test]
fn modal_choice_preserves_composer() {
    let mut app = TuiApp::new();
    app.paste("draft");
    app.set_approval(Some("run command?".to_string()));
    assert_eq!(app.handle_key(key(KeyCode::Char('y'))), InputAction::None);
    assert_eq!(app.take_approval_choice(), Some(ApprovalChoice::Yes));
    assert_eq!(app.input_text(), "draft");
    assert!(app.approval.is_none());
}

#[test]
fn approval_denial_and_escape_are_reported() {
    let mut app = TuiApp::new();
    for (event, expected) in [
        (key(KeyCode::Enter), ApprovalChoice::No),
        (key(KeyCode::Esc), ApprovalChoice::Esc),
        (
            modified(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ApprovalChoice::Esc,
        ),
    ] {
        app.set_approval(Some("approve?".to_string()));
        let _ = app.handle_key(event);
        assert_eq!(app.take_approval_choice(), Some(expected));
    }
}

#[test]
fn page_keys_adjust_transcript_scroll() {
    let mut app = TuiApp::new();
    assert_eq!(app.scroll_from_bottom, 0);
    let _ = app.handle_key(key(KeyCode::PageUp));
    assert_eq!(app.scroll_from_bottom, 5);
    let _ = app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.scroll_from_bottom, 0);
}

#[test]
fn transcript_scroll_is_clamped_after_render() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "one line".to_string(),
    });
    app.scroll_from_bottom = usize::MAX;
    let _ = render(&mut app, 60, 18);
    assert_eq!(app.scroll_from_bottom, 0);
}

#[test]
fn streamed_output_keeps_scrolled_transcript_anchored() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..30)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    let _ = render(&mut app, 40, 12);
    app.scroll_from_bottom = 5;
    let _ = render(&mut app, 40, 12);
    let before = app.scroll_from_bottom;

    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "new one\nnew two\nnew three".to_string(),
    });
    let _ = render(&mut app, 40, 12);

    assert!(app.scroll_from_bottom > before);
}

#[test]
fn main_regions_render_in_vertical_order() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Header("HEADER-MARK".to_string()));
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "TRANSCRIPT-MARK".to_string(),
    });
    app.set_activity(Some("STATUS-MARK".to_string()));
    app.paste("COMPOSER-MARK");

    let buffer = render(&mut app, 80, 24);
    let header = marker_row(&buffer, "HEADER-MARK");
    let transcript = marker_row(&buffer, "TRANSCRIPT-MARK");
    let status = marker_row(&buffer, "STATUS-MARK");
    let composer = marker_row(&buffer, "COMPOSER-MARK");
    let footer = marker_row(&buffer, "Enter send");
    assert!(header < transcript);
    assert!(transcript < status);
    assert!(status < composer);
    assert!(composer < footer);
}

#[test]
fn spinner_tick_only_changes_status_region() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "stable transcript".to_string(),
    });
    app.paste("stable input");
    app.set_activity(Some("thinking".to_string()));
    let before = render(&mut app, 60, 18);
    app.tick();
    let after = render(&mut app, 60, 18);
    let status = layout_areas(Rect::new(0, 0, 60, 18), app.composer_height()).status;

    let mut changes = 0;
    for y in 0..18 {
        for x in 0..60 {
            if before[(x, y)] != after[(x, y)] {
                changes += 1;
                assert!(x >= status.x && x < status.right());
                assert!(y >= status.y && y < status.bottom());
            }
        }
    }
    assert!(changes > 0);
}

#[test]
fn approval_modal_renders_over_base_view() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "base transcript".to_string(),
    });
    app.set_approval(Some("Allow write_file?".to_string()));
    let buffer = render(&mut app, 60, 18);
    assert!(marker_row(&buffer, "Approval required") > 0);
    assert!(marker_row(&buffer, "Allow write_file?") > 0);
}

#[test]
fn long_approval_prompt_can_scroll_to_suffix() {
    let mut app = TuiApp::new();
    let prompt = (0..20)
        .map(|index| format!("command part {index}"))
        .chain(["DANGEROUS-SUFFIX".to_string()])
        .collect::<Vec<_>>()
        .join("\n");
    app.set_approval(Some(prompt));
    let first = render(&mut app, 50, 12);
    assert!(!(0..first.area.height).any(|y| row(&first, y).contains("DANGEROUS-SUFFIX")));
    for _ in 0..10 {
        let _ = app.handle_key(key(KeyCode::PageDown));
    }
    let scrolled = render(&mut app, 50, 12);
    assert!((0..scrolled.area.height).any(|y| row(&scrolled, y).contains("DANGEROUS-SUFFIX")));
}

#[test]
fn tiny_and_narrow_terminals_render_without_panicking() {
    for (width, height) in [(1, 1), (4, 3), (12, 5), (20, 8)] {
        let mut app = TuiApp::new();
        app.apply_output(OutputEvent::Stream {
            kind: StreamKind::Assistant,
            delta: "你好 **world**".to_string(),
        });
        app.paste("wide 你好");
        let buffer = render(&mut app, width, height);
        assert_eq!(buffer.area, Rect::new(0, 0, width, height));
    }
}
