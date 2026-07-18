use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::app::{InputAction, TuiApp};
use super::composer;
use super::view::{LayoutAreas, layout_areas};

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

fn buffer_contains(buffer: &Buffer, marker: &str) -> bool {
    (0..buffer.area.height).any(|y| row(buffer, y).contains(marker))
}

fn areas(app: &TuiApp, area: Rect) -> LayoutAreas {
    let metrics = composer::measure(&app.composer, area);
    layout_areas(area, metrics.outer_height)
}

fn assert_multiline_paste(pasted: &str) {
    let mut app = TuiApp::new();
    app.paste(pasted);

    assert_multiline_contents(&app);
    assert_multiline_render(&mut app);
    assert_multiline_submit(&mut app);
}

fn assert_multiline_contents(app: &TuiApp) {
    assert_eq!(app.input_text(), "- alpha\n\n  - beta\n");
    assert_eq!(app.composer.lines(), ["- alpha", "", "  - beta", ""]);
    assert_eq!(app.composer.cursor(), (3, 0));
    let metrics = composer::measure(&app.composer, Rect::new(0, 0, 40, 18));
    assert_eq!(metrics.visual_rows, 4);
    assert_eq!(metrics.outer_height, 6);
}

fn assert_multiline_render(app: &mut TuiApp) {
    let buffer = render(app, 40, 18);
    assert!(buffer_contains(&buffer, "- alpha"));
    assert!(buffer_contains(&buffer, "  - beta"));
}

fn assert_multiline_submit(app: &mut TuiApp) {
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        InputAction::Submit("- alpha\n\n  - beta\n".to_string())
    );
}

#[test]
fn paste_preserves_multiline_whitespace_for_common_newline_formats() {
    for pasted in [
        "- alpha\n\n  - beta\n",
        "- alpha\r\n\r\n  - beta\r\n",
        "- alpha\r\r  - beta\r",
    ] {
        assert_multiline_paste(pasted);
    }
}

#[test]
fn composer_grows_for_soft_wrapped_input() {
    let mut app = TuiApp::new();
    app.paste(
        &(0..30)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let buffer = render(&mut app, 20, 24);
    let metrics = composer::measure(&app.composer, Rect::new(0, 0, 20, 24));
    let composer = layout_areas(Rect::new(0, 0, 20, 24), metrics.outer_height).composer;

    assert!(metrics.visual_rows > 5);
    assert_eq!(metrics.outer_height, 7);
    assert!(
        (composer.y + 1..composer.bottom() - 1)
            .any(|y| buffer[(composer.right() - 1, y)].symbol() == "█")
    );
}

#[test]
fn overflowing_composer_renders_an_internal_scrollbar() {
    let mut app = overflowing_app();
    let buffer = render(&mut app, 40, 18);
    let composer = areas(&app, Rect::new(0, 0, 40, 18)).composer;
    let x = composer.right() - 1;
    assert_eq!(buffer[(x, composer.bottom() - 2)].symbol(), "█");
    assert!((0..buffer.area.height).any(|y| row(&buffer, y).contains("draft line 19")));
    assert!(!(0..buffer.area.height).any(|y| row(&buffer, y).contains("draft line 0")));

    app.composer.move_cursor(ratatui_textarea::CursorMove::Top);
    let buffer = render(&mut app, 40, 18);
    assert_eq!(buffer[(x, composer.y + 1)].symbol(), "█");
    assert!((0..buffer.area.height).any(|y| row(&buffer, y).contains("draft line 0")));
    assert!(!(0..buffer.area.height).any(|y| row(&buffer, y).contains("draft line 19")));
}

#[test]
fn composer_page_scroll_does_not_overshoot_the_last_page() {
    let mut app = overflowing_app();
    let _ = render(&mut app, 40, 18);
    let _ = app.handle_key(modified(KeyCode::Char('v'), KeyModifiers::CONTROL));
    let buffer = render(&mut app, 40, 18);

    assert!((0..buffer.area.height).any(|y| row(&buffer, y).contains("draft line 15")));
    assert!((0..buffer.area.height).any(|y| row(&buffer, y).contains("draft line 19")));
}

#[test]
fn tiny_borderless_composer_uses_its_actual_wrap_width() {
    let mut app = TuiApp::new();
    app.paste(&"x".repeat(39));
    let _ = render(&mut app, 20, 6);
    let _ = app.handle_key(modified(KeyCode::Char('v'), KeyModifiers::CONTROL));
    let buffer = render(&mut app, 20, 6);

    let visible = (0..buffer.area.height)
        .map(|y| row(&buffer, y).matches('x').count())
        .sum::<usize>();
    assert_eq!(visible, 39);
}

#[test]
fn tiny_composer_keeps_input_visible_without_a_border() {
    for (width, height) in [(1, 1), (2, 2), (20, 6)] {
        let mut app = TuiApp::new();
        app.paste("X");
        let buffer = render(&mut app, width, height);

        assert!((0..height).any(|y| row(&buffer, y).contains('X')));
    }
}

fn overflowing_app() -> TuiApp {
    let mut app = TuiApp::new();
    app.paste(
        &(0..20)
            .map(|index| format!("draft line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    app
}
