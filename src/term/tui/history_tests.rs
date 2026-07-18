use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::app::{InputAction, TuiApp};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn modified(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn render(app: &mut TuiApp, width: u16, height: u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
}

fn assert_composer(app: &TuiApp, text: &str, cursor: (usize, usize)) {
    assert_eq!(app.input_text(), text);
    assert_eq!(app.composer.cursor(), cursor);
}

#[test]
fn arrow_history_navigation_restores_unfinished_draft() {
    let mut app = TuiApp::new();
    app.paste("older");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("newer");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("unfinished draft");

    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "newer", (0, 5));
    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "older", (0, 5));
    let _ = app.handle_key(key(KeyCode::Down));
    assert_composer(&app, "newer", (0, 5));
    let _ = app.handle_key(key(KeyCode::Down));
    assert_composer(&app, "unfinished draft", (0, 16));
}

#[test]
fn arrows_move_through_draft_lines_before_history() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("first line\nsecond line");

    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "first line\nsecond line", (0, 10));
    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "prior", (0, 5));
    let _ = app.handle_key(key(KeyCode::Down));
    assert_composer(&app, "first line\nsecond line", (0, 10));
    let _ = app.handle_key(key(KeyCode::Down));
    assert_composer(&app, "first line\nsecond line", (1, 10));
}

#[test]
fn arrows_move_through_recalled_multiline_prompt_before_draft() {
    let mut app = TuiApp::new();
    app.paste("recalled first\nrecalled second");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("unfinished");

    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "recalled first\nrecalled second", (1, 15));
    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "recalled first\nrecalled second", (0, 14));
    let _ = app.handle_key(key(KeyCode::Up));
    assert_composer(&app, "recalled first\nrecalled second", (0, 14));
    let _ = app.handle_key(key(KeyCode::Down));
    assert_composer(&app, "recalled first\nrecalled second", (1, 14));
    let _ = app.handle_key(key(KeyCode::Down));
    assert_composer(&app, "unfinished", (0, 10));
}

#[test]
fn editing_recalled_prompt_detaches_it_from_history() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("unfinished");

    let _ = app.handle_key(key(KeyCode::Up));
    let _ = app.handle_key(key(KeyCode::Char('!')));
    let _ = app.handle_key(key(KeyCode::Down));

    assert_composer(&app, "prior!", (0, 6));
}

#[test]
fn arrows_move_through_soft_wrapped_rows_before_history() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    let draft = "one two three four five six seven eight nine";
    app.paste(draft);
    render(&mut app, 20, 12);
    let bottom_row = app.composer.screen_cursor().row;
    assert!(bottom_row > 0);

    let _ = app.handle_key(key(KeyCode::Up));

    assert_eq!(app.input_text(), draft);
    assert_eq!(app.composer.screen_cursor().row, bottom_row - 1);
}

#[test]
fn active_selection_is_cleared_with_submitted_prompt() {
    let mut app = TuiApp::new();
    app.paste("abcde");
    app.composer
        .move_cursor(ratatui_textarea::CursorMove::Jump(0, 2));
    let _ = app.handle_key(modified(KeyCode::Right, KeyModifiers::SHIFT));

    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        InputAction::Submit("abcde".to_string())
    );
    assert!(app.composer.is_empty());
}

#[test]
fn alt_arrow_traverses_history_from_inside_multiline_draft() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("first\nsecond");

    let _ = app.handle_key(modified(KeyCode::Up, KeyModifiers::ALT));

    assert_composer(&app, "prior", (0, 5));
}

#[test]
fn history_restores_draft_selection_and_undo_state() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("unfinished");
    app.composer
        .move_cursor(ratatui_textarea::CursorMove::Jump(0, 2));
    let _ = app.handle_key(modified(KeyCode::Right, KeyModifiers::SHIFT));

    let _ = app.handle_key(modified(KeyCode::Up, KeyModifiers::ALT));
    let _ = app.handle_key(modified(KeyCode::Down, KeyModifiers::ALT));

    assert_eq!(app.composer.selection_range(), Some(((0, 2), (0, 3))));
    let _ = app.handle_key(modified(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(app.composer.is_empty());
}

#[test]
fn combined_alt_arrows_keep_native_textarea_movement() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("first\nsecond");

    let _ = app.handle_key(modified(
        KeyCode::Up,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));

    assert_composer(&app, "first\nsecond", (0, 5));
}

#[test]
fn normalized_empty_paste_detaches_recalled_prompt() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("unfinished");
    let _ = app.handle_key(modified(KeyCode::Up, KeyModifiers::ALT));
    app.composer
        .move_cursor(ratatui_textarea::CursorMove::Jump(0, 2));
    let _ = app.handle_key(modified(KeyCode::Right, KeyModifiers::SHIFT));

    app.paste("");
    let _ = app.handle_key(key(KeyCode::Down));

    assert_composer(&app, "pror", (0, 2));
}

#[test]
fn active_selection_at_boundary_does_not_open_history() {
    let mut app = TuiApp::new();
    app.paste("prior");
    let _ = app.handle_key(key(KeyCode::Enter));
    app.paste("unfinished");
    app.composer
        .move_cursor(ratatui_textarea::CursorMove::Jump(0, 0));
    let _ = app.handle_key(modified(KeyCode::Right, KeyModifiers::SHIFT));

    let _ = app.handle_key(key(KeyCode::Up));

    assert_composer(&app, "unfinished", (0, 1));
    assert!(app.composer.is_selecting());
}
