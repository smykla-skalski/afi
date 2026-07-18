use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::term::{MessageKind, OutputEvent, StreamKind};

use super::app::TuiApp;
use super::composer;
use super::transcript;
use super::view::layout_areas;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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

fn append_stream_burst(app: &mut TuiApp) {
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "new".to_string(),
    });
    for _ in 0..100 {
        app.apply_output(OutputEvent::Stream {
            kind: StreamKind::Assistant,
            delta: " delta".to_string(),
        });
    }
}

fn populate_cache_fixture(app: &mut TuiApp) {
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "Finished **entry**".to_string(),
    });
    app.apply_output(OutputEvent::StreamFinished);
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "stable info".to_string(),
    });
}

fn cache_fixture_with_burst() -> TuiApp {
    let mut app = TuiApp::new();
    populate_cache_fixture(&mut app);
    append_stream_burst(&mut app);
    app
}

fn contains(buffer: &Buffer, text: &str) -> bool {
    (0..buffer.area.height).any(|y| row(buffer, y).contains(text))
}

fn assert_tail_cache_work(initial: transcript::CacheStats, streamed: transcript::CacheStats) {
    assert_eq!(streamed.formatted_entries, initial.formatted_entries + 1);
    assert_eq!(streamed.markdown_parses, initial.markdown_parses);
    assert_eq!(streamed.measured_entries, initial.measured_entries + 1);
    assert_eq!(streamed.content_checks, initial.content_checks + 1);
    assert_eq!(streamed.offset_updates, initial.offset_updates + 1);
    assert_eq!(
        streamed.stream_input_bytes,
        initial.stream_input_bytes + 603
    );
}

fn assert_resize_cache_work(
    streamed: transcript::CacheStats,
    resized: transcript::CacheStats,
    entries: usize,
) {
    assert_eq!(resized.formatted_entries, streamed.formatted_entries);
    assert_eq!(resized.markdown_parses, streamed.markdown_parses);
    assert_eq!(resized.stream_input_bytes, streamed.stream_input_bytes);
    assert_eq!(
        resized.measured_entries,
        streamed.measured_entries + entries
    );
}

fn assert_stream_input(app: &TuiApp, markdown_parses: usize, input_bytes: usize) {
    let stats = app.transcript_view.stats();
    assert_eq!(stats.markdown_parses, markdown_parses);
    assert_eq!(stats.stream_input_bytes, input_bytes);
}

fn rendered_stream(parts: &[&str]) -> (Buffer, Option<usize>) {
    let mut app = TuiApp::new();
    let mut buffer = render(&mut app, 24, 12);
    for part in parts {
        app.apply_output(OutputEvent::Stream {
            kind: StreamKind::Assistant,
            delta: (*part).to_string(),
        });
        buffer = render(&mut app, 24, 12);
    }
    (buffer, app.transcript_scroll_limit)
}

fn assert_split_stream_matches(parts: &[&str], combined: &str) {
    let warm = rendered_stream(parts);
    let cold = rendered_stream(&[combined]);
    assert_eq!(warm.0, cold.0);
    assert_eq!(warm.1, cold.1);
}

fn assert_modal_backdrop(buffer: &Buffer) {
    assert!(marker_row(buffer, "Approval required") > 0);
    assert!(marker_row(buffer, "Allow write_file?") > 0);
    assert!(buffer[(0, 0)].modifier.contains(Modifier::DIM));
}

fn assert_modal_surface(buffer: &Buffer) {
    let border = &buffer[(1, 6)];
    assert_eq!(border.fg, Color::Yellow);
    assert_eq!(border.bg, Color::Black);
    assert!(border.modifier.contains(Modifier::BOLD));

    let surface = &buffer[(2, 7)];
    assert_eq!(surface.bg, Color::Black);
    assert!(!surface.modifier.contains(Modifier::DIM));
    assert_eq!(buffer[(59, 7)].symbol(), "▓");
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
    let metrics = composer::measure(&app.composer, Rect::new(0, 0, 60, 18));
    let status = layout_areas(Rect::new(0, 0, 60, 18), metrics.outer_height).status;

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
fn unchanged_frames_reuse_transcript_and_composer_work() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "A **cached** response".to_string(),
    });
    app.set_activity(Some("thinking".to_string()));
    let _ = render(&mut app, 60, 18);
    let transcript = app.transcript_view.stats();
    let composer = app.composer_view.measurement_count();

    app.tick();
    let _ = render(&mut app, 60, 18);

    assert_eq!(app.transcript_view.stats(), transcript);
    assert_eq!(app.composer_view.measurement_count(), composer);
}

#[test]
fn transcript_cache_invalidates_only_affected_work() {
    let mut app = TuiApp::new();
    populate_cache_fixture(&mut app);
    let _ = render(&mut app, 60, 18);
    let initial = app.transcript_view.stats();

    append_stream_burst(&mut app);
    let streamed_buffer = render(&mut app, 60, 18);
    let streamed = app.transcript_view.stats();
    assert_tail_cache_work(initial, streamed);
    assert!(contains(&streamed_buffer, "delta"));
    let mut cold = cache_fixture_with_burst();
    assert_eq!(streamed_buffer, render(&mut cold, 60, 18));

    let resized_buffer = render(&mut app, 72, 18);
    let resized = app.transcript_view.stats();
    assert_resize_cache_work(streamed, resized, app.transcript.len());
    let mut cold = cache_fixture_with_burst();
    assert_eq!(resized_buffer, render(&mut cold, 72, 18));

    app.apply_output(OutputEvent::StreamFinished);
    let _ = render(&mut app, 72, 18);
    assert_eq!(
        app.transcript_view.stats().markdown_parses,
        resized.markdown_parses + 1
    );
}

#[test]
fn active_markdown_is_parsed_once_when_stream_finishes() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "**bo".to_string(),
    });
    let active = render(&mut app, 40, 12);
    assert!(contains(&active, "**bo"));
    assert_stream_input(&app, 0, 4);

    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "ld**".to_string(),
    });
    let _ = render(&mut app, 40, 12);
    assert_stream_input(&app, 0, 8);

    app.apply_output(OutputEvent::StreamFinished);
    let finished = render(&mut app, 40, 12);
    assert!(contains(&finished, "bold"));
    assert!(!contains(&finished, "**bold**"));
    assert_stream_input(&app, 1, 8);
}

#[test]
fn streaming_resegments_cross_delta_graphemes_and_newlines() {
    assert_split_stream_matches(&["prefix 👩", "\u{200d}💻 suffix"], "prefix 👩‍💻 suffix");
    assert_split_stream_matches(&["prefix 👍", "🏽 suffix"], "prefix 👍🏽 suffix");
    assert_split_stream_matches(&["line one\r", "\nline two"], "line one\r\nline two");
}

#[test]
fn streaming_word_wrap_matches_the_completed_paragraph() {
    for text in [
        "alpha beta gamma delta epsilon",
        "alpha   beta supercalifragilistic",
        "abcdefghijkl\tmonp",
        "ｶﾞｶﾞｶﾞｶﾞｶﾞｶﾞｶﾞ",
        "line one\n",
        "line one\n\n",
        "line one\r",
    ] {
        let mut app = TuiApp::new();
        app.apply_output(OutputEvent::Stream {
            kind: StreamKind::Reasoning,
            delta: text.to_string(),
        });
        let active = render(&mut app, 24, 12);
        app.apply_output(OutputEvent::StreamFinished);
        let completed = render(&mut app, 24, 12);
        assert_eq!(active, completed, "word-wrap mismatch for {text:?}");
    }
}

#[test]
fn completed_markdown_shrink_keeps_scrolled_content_anchored() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..30)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    app.apply_output(OutputEvent::Stream {
        kind: StreamKind::Assistant,
        delta: "**12345678901234**".to_string(),
    });
    let _ = render(&mut app, 30, 12);
    app.scroll_from_bottom = 5;
    let _ = render(&mut app, 30, 12);
    let active_distance = app.scroll_from_bottom;

    app.apply_output(OutputEvent::StreamFinished);
    let _ = render(&mut app, 30, 12);

    assert_eq!(app.scroll_from_bottom, active_distance - 1);
}

#[test]
fn resizing_preserves_the_scrolled_top_row() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: (0..40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    let _ = render(&mut app, 40, 12);
    app.scroll_from_bottom = 5;
    let _ = render(&mut app, 40, 12);
    let before_top = app.transcript_scroll_limit.unwrap() - app.scroll_from_bottom;

    let _ = render(&mut app, 40, 14);
    let after_top = app.transcript_scroll_limit.unwrap() - app.scroll_from_bottom;

    assert_eq!(after_top, before_top);
}

#[test]
fn width_resize_keeps_the_same_entry_in_view() {
    let mut app = TuiApp::new();
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Info,
        text: "A".repeat(500),
    });
    app.apply_output(OutputEvent::Message {
        kind: MessageKind::Warning,
        text: "SECOND-MARKER".to_string(),
    });
    let _ = render(&mut app, 20, 12);
    app.scroll_from_bottom = 4;
    let narrow = render(&mut app, 20, 12);
    assert!(contains(&narrow, "AAAA"));

    let wide = render(&mut app, 40, 12);

    assert!(contains(&wide, "AAAA"));
    assert!(!contains(&wide, "SECOND-MARKER"));
}

#[test]
fn composer_measurement_cache_tracks_edits_and_width() {
    let mut app = TuiApp::new();
    let _ = render(&mut app, 60, 18);
    let initial = app.composer_view.measurement_count();
    let _ = render(&mut app, 60, 18);
    assert_eq!(app.composer_view.measurement_count(), initial);

    let _ = app.handle_key(key(KeyCode::Left));
    let _ = render(&mut app, 60, 18);
    assert_eq!(app.composer_view.measurement_count(), initial);

    app.paste("edit");
    let _ = render(&mut app, 60, 18);
    assert_eq!(app.composer_view.measurement_count(), initial + 1);

    let _ = render(&mut app, 72, 18);
    assert_eq!(app.composer_view.measurement_count(), initial + 2);
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
    assert_modal_backdrop(&buffer);
    assert_modal_surface(&buffer);
}

#[test]
fn approval_suspends_background_animation() {
    let mut app = TuiApp::new();
    app.set_task_running(true);
    assert!(app.should_animate());
    app.set_approval(Some("approve?".to_string()));
    assert!(!app.should_animate());
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
    assert!((0..first.area.height).any(|y| first[(48, y)].symbol() == "█"));
    for _ in 0..10 {
        let _ = app.handle_key(key(KeyCode::PageDown));
    }
    let scrolled = render(&mut app, 50, 12);
    assert!((0..scrolled.area.height).any(|y| row(&scrolled, y).contains("DANGEROUS-SUFFIX")));
}

#[test]
fn small_viable_terminal_keeps_modal_prompt_visible() {
    let mut app = TuiApp::new();
    app.set_approval(Some("Allow?".to_string()));
    let buffer = render(&mut app, 12, 3);
    assert!(contains(&buffer, "Allow?"));
    assert!(contains(&buffer, "Y yes"));
}
