use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::risk::ApprovalChoice;
use crate::term::{MessageKind, OutputEvent, StreamKind};

use super::app::{InputAction, TuiApp};
use super::composer;
use super::view::layout_areas;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn modified(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn mouse(kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

fn released(code: KeyCode) -> KeyEvent {
    KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Release)
}

fn render(app: &mut TuiApp, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn areas(app: &TuiApp, area: Rect) -> super::view::LayoutAreas {
    let metrics = composer::measure(&app.composer, area);
    layout_areas(area, metrics.outer_height)
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
fn mouse_wheel_scrolls_transcript_and_approval() {
    let mut app = TuiApp::new();
    app.handle_mouse(mouse(MouseEventKind::ScrollUp));
    assert_eq!(app.scroll_from_bottom, 5);
    app.handle_mouse(mouse(MouseEventKind::ScrollDown));
    assert_eq!(app.scroll_from_bottom, 0);

    app.set_approval(Some("inspect command".to_string()));
    app.handle_mouse(mouse(MouseEventKind::ScrollDown));
    assert_eq!(app.approval_scroll, 5);
    app.handle_mouse(mouse(MouseEventKind::ScrollUp));
    assert_eq!(app.approval_scroll, 0);
}

#[test]
fn ignored_input_does_not_request_a_redraw() {
    let mut app = TuiApp::new();
    let (_, redraw) = app.handle_key_with_redraw(released(KeyCode::Char('x')));
    assert!(!redraw);
    assert!(!app.handle_mouse_with_redraw(mouse(MouseEventKind::Moved)));

    app.set_task_running(true);
    let (_, redraw) = app.handle_key_with_redraw(key(KeyCode::Char('x')));
    assert!(!redraw);
    assert!(!app.paste_with_redraw("blocked"));

    app.set_approval(Some("approve?".to_string()));
    let (_, redraw) = app.handle_key_with_redraw(key(KeyCode::Char('x')));
    assert!(!redraw);
}

#[test]
fn meaningful_input_requests_a_redraw() {
    let mut app = TuiApp::new();
    let (_, redraw) = app.handle_key_with_redraw(key(KeyCode::Char('x')));
    assert!(redraw);
    assert!(app.handle_mouse_with_redraw(mouse(MouseEventKind::ScrollUp)));
    assert!(app.paste_with_redraw(" pasted"));
}

#[test]
fn scrolling_against_a_rendered_boundary_skips_redraw() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..30)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    app.scroll_from_bottom = usize::MAX;
    let _ = render(&mut app, 40, 12);
    assert!(!app.handle_mouse_with_redraw(mouse(MouseEventKind::ScrollUp)));

    app.set_approval(Some(
        (0..30)
            .map(|line| format!("part {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    app.approval_scroll = usize::MAX;
    let _ = render(&mut app, 40, 12);
    assert!(!app.handle_mouse_with_redraw(mouse(MouseEventKind::ScrollDown)));
    let (_, redraw) = app.handle_key_with_redraw(key(KeyCode::PageDown));
    assert!(!redraw);
}

#[test]
fn new_output_reopens_scroll_before_the_coalesced_frame() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "short".to_string(),
    });
    let _ = render(&mut app, 40, 12);
    assert_eq!(app.transcript_scroll_limit, Some(0));

    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..30)
            .map(|line| format!("new line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    assert!(app.handle_mouse_with_redraw(mouse(MouseEventKind::ScrollUp)));
    let _ = render(&mut app, 40, 12);
    assert_eq!(app.scroll_from_bottom, 5);
}

#[test]
fn pending_output_combines_existing_anchor_with_new_scroll() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..30)
            .map(|line| format!("old line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    let _ = render(&mut app, 40, 12);
    app.scroll_up();
    let _ = render(&mut app, 40, 12);
    let old_limit = app.transcript_scroll_limit.unwrap();

    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..10)
            .map(|line| format!("new line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    app.scroll_up();
    let _ = render(&mut app, 40, 12);
    let growth = app.transcript_scroll_limit.unwrap() - old_limit;

    assert_eq!(app.scroll_from_bottom, 10 + growth);
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
fn scrollbar_thumb_tracks_conversation_position() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..30)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });

    let bottom = render(&mut app, 40, 12);
    let transcript = areas(&app, Rect::new(0, 0, 40, 12)).transcript;
    let x = transcript.right() - 1;
    let top_y = transcript.y + 1;
    let bottom_y = transcript.bottom() - 2;
    assert_eq!(bottom[(x, bottom_y)].symbol(), "█");

    app.scroll_from_bottom = usize::MAX;
    let top = render(&mut app, 40, 12);
    assert_eq!(top[(x, top_y)].symbol(), "█");
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
fn tiny_and_narrow_terminals_render_without_panicking() {
    for (width, height) in [(1, 1), (1, 12), (2, 12), (4, 3), (12, 5), (20, 8)] {
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
