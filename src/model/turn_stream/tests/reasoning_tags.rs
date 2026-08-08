//! Accumulator tests for reasoning the model wrote into `content`: the cut,
//! the take-back of a tool call the splitter routed off the answer, and the
//! provenance rule that keeps a quoted one out of it.
//!
//! Apart from `tests.rs`, which covers the fold itself, only because the two
//! together run past the repository per-file line cap.

use super::*;

/// Reasoning wrapped in tags reaches the cut the same way the field does, and
/// keeps reaching it across spans.
///
/// The whitespace between two spans is the trap: emitted as content it would
/// fill `content_parts`, and `no_output_yet` reads that as the answer having
/// started - so nothing would count toward the limit again and a looping model
/// would run to stream end.
#[test]
fn tagged_reasoning_reaches_the_cut_across_spans() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(12, true);
    let mut push = |text: &str, accumulator: &mut StreamAccumulator| {
        accumulator.push(
            &StreamChunk {
                content: Some(text.to_string()),
                ..StreamChunk::default()
            },
            Instant::now(),
            &mut ui,
        )
    };

    // Seven characters, then five, against a limit of twelve.
    assert!(
        push("<think>1234567</think>\n\n", &mut accumulator).is_none(),
        "under the limit, and the newlines are not an answer"
    );
    let result = push("<think>89abc</think>", &mut accumulator)
        .expect("the second span must reach the limit");
    let StreamResult::ReasoningStall { chars, .. } = result else {
        panic!("expected reasoning stall");
    };
    assert_eq!(chars, 12, "both spans counted, the whitespace did not");
}

/// A server with no native tool calling puts its call in the message body, and
/// a model there can emit it inside a span. The splitter routes that off the
/// answer, and `parse_text_calls` only reads the answer - so without taking it
/// back the call is never dispatched and the turn reports a reasoning stall.
#[test]
fn a_tool_call_captured_as_reasoning_is_taken_back() {
    let call =
        "[afi_tool_call]{\"name\":\"list_dir\",\"arguments\":{\"path\":\".\"}}[/afi_tool_call]";
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(0, true);
    accumulator.push(
        &StreamChunk {
            content: Some(format!("<think>I should look. {call}</think>")),
            ..StreamChunk::default()
        },
        Instant::now(),
        &mut ui,
    );

    let StreamResult::Done(acc) = accumulator.finish(&mut ui) else {
        panic!("expected a completed turn");
    };
    assert!(acc.content_parts.join("").is_empty(), "still reasoning");
    assert!(
        acc.answer_text().contains("[afi_tool_call]"),
        "the dispatcher must see it: {:?}",
        acc.answer_text()
    );
}

/// Reasoning a provider reported in its own field is never a candidate for the
/// answer, whatever it happens to contain.
///
/// afi's system prompt carries unfenced text-protocol examples with real tool
/// names, so a model restating its instructions in a scratchpad reproduces one
/// byte for byte. Taking that back would run a tool nobody asked for and commit
/// the raw chain-of-thought to the conversation as an assistant message. The
/// splitter is off here, as it is for Anthropic - but the field is read on every
/// source, so the flag is not what makes this safe.
#[test]
fn a_quoted_call_in_reported_reasoning_is_never_taken_back() {
    let quoted = "I could emit [afi_tool_call]{\"name\":\"read_file\",\
                  \"arguments\":{\"path\":\"foo.py\"}}[/afi_tool_call] but I will not";
    for split_tags in [false, true] {
        let mut ui = RecordingUi::default();
        let mut accumulator = StreamAccumulator::new(0, split_tags);
        accumulator.push(
            &StreamChunk {
                reasoning_content: Some(quoted.to_string()),
                ..StreamChunk::default()
            },
            Instant::now(),
            &mut ui,
        );

        let acc = finish(accumulator, &mut ui);
        assert_eq!(acc.reasoning_parts.join(""), quoted, "still reported");
        assert_eq!(acc.answer_text(), "", "not an answer (tags={split_tags})");
    }
}

/// The answer survives the cut when both arrive in one delta.
///
/// A delta can carry the end of a span and the start of the reply. Stalling on
/// the reasoning before emitting the content would discard text the model had
/// already answered with, then nudge it to act - which it just had.
#[test]
fn an_answer_sharing_the_cutting_delta_is_kept() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(4, true);
    let outcome = accumulator.push(
        &StreamChunk {
            content: Some("<think>long enough</think>The answer is 42.".to_string()),
            ..StreamChunk::default()
        },
        Instant::now(),
        &mut ui,
    );

    assert!(outcome.is_none(), "the model answered; it must not stall");
    let acc = finish(accumulator, &mut ui);
    assert_eq!(acc.content_parts.join(""), "The answer is 42.");
}

/// Reasoning without a call is left where it is, so a turn that really did
/// nothing but deliberate still reports as one.
#[test]
fn reasoning_without_a_call_is_not_taken_back() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(0, true);
    accumulator.push(
        &StreamChunk {
            content: Some("<think>just deliberating</think>".to_string()),
            ..StreamChunk::default()
        },
        Instant::now(),
        &mut ui,
    );

    let StreamResult::Done(acc) = accumulator.finish(&mut ui) else {
        panic!("expected a completed turn");
    };
    assert_eq!(acc.answer_text(), "");
}
