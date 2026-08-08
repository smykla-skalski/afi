use super::{ReasoningTags, Split, hold_partial, strip};

/// Feed a whole stream and return what the caller would have accumulated.
fn run(deltas: &[&str]) -> Split {
    let mut tags = ReasoningTags::new(true);
    let mut all = Split::default();
    for delta in deltas {
        let split = tags.split(delta);
        all.reasoning.push_str(&split.reasoning);
        all.content.push_str(&split.content);
    }
    let last = tags.flush();
    all.reasoning.push_str(&last.reasoning);
    all.content.push_str(&last.content);
    all
}

/// The shape Bedrock actually streams: every delta carries its own matched
/// pair. Captured from `openai.gpt-oss-120b-1:0` in `us-west-2`.
#[test]
fn each_delta_carrying_its_own_pair_is_all_reasoning() {
    let out = run(&[
        "",
        "<reasoning>We need to parse riddle. Answer: 9</reasoning>",
        "<reasoning>. Provide answer.</reasoning>",
        "**Step-by-step reasoning**\n\n1. The farmer starts with 17 sheep.",
    ]);
    assert_eq!(
        out.reasoning,
        "We need to parse riddle. Answer: 9. Provide answer."
    );
    assert_eq!(
        out.content,
        "**Step-by-step reasoning**\n\n1. The farmer starts with 17 sheep."
    );
}

/// The other shape: one span opened and closed across many deltas, which is
/// what a reasoning model on llama.cpp or vLLM emits.
#[test]
fn one_span_across_many_deltas_is_all_reasoning() {
    let out = run(&["<think>Let me", " work through", " it.</think>", "42"]);
    assert_eq!(out.reasoning, "Let me work through it.");
    assert_eq!(out.content, "42");
}

/// A tag cut at an arbitrary byte: neither delta contains it, so recognising it
/// at all needs the held-back tail.
#[test]
fn a_tag_split_across_deltas_is_still_recognised() {
    let out = run(&["<reas", "oning>hidden</reason", "ing>shown"]);
    assert_eq!(out.reasoning, "hidden");
    assert_eq!(out.content, "shown");
}

/// The guard against eating a reply. Once content has been streamed, a tag is
/// text the model wrote, not a channel marker.
#[test]
fn a_tag_after_the_answer_starts_stays_in_the_answer() {
    let out = run(&["Servers emit ", "<think>x</think>", " around reasoning."]);
    assert_eq!(out.reasoning, "");
    assert_eq!(
        out.content,
        "Servers emit <think>x</think> around reasoning."
    );
}

/// The same guard within a single delta, where the answer and the tag arrive
/// together.
#[test]
fn a_tag_behind_content_in_one_delta_stays_in_the_answer() {
    let out = run(&["Use <reasoning> for this.</reasoning>"]);
    assert_eq!(out.reasoning, "");
    assert_eq!(out.content, "Use <reasoning> for this.</reasoning>");
}

/// A close belonging to the other pair does not end the span, so a model
/// discussing one tag inside the other is not cut short.
#[test]
fn only_the_matching_close_ends_a_span() {
    let out = run(&["<think>about </reasoning> tags</think>answer"]);
    assert_eq!(out.reasoning, "about </reasoning> tags");
    assert_eq!(out.content, "answer");
}

/// Deliberation that never closes is still deliberation. Calling it content
/// would put the model's notes in `summary.answer`, which is the whole failure
/// being prevented.
#[test]
fn an_unterminated_span_flushes_as_reasoning() {
    let out = run(&["<reasoning>cut off mid-thou"]);
    assert_eq!(out.reasoning, "cut off mid-thou");
    assert_eq!(out.content, "");
}

/// A dangling prefix is text the model sent, so it is released rather than
/// swallowed when the stream ends.
#[test]
fn a_dangling_tag_prefix_flushes_as_content() {
    let out = run(&["answer<rea"]);
    assert_eq!(out.reasoning, "");
    assert_eq!(out.content, "answer<rea");
}

/// A stream with no tags at all must come through untouched, which is every
/// model that reports reasoning in its own field - or none.
#[test]
fn an_untagged_stream_is_unchanged() {
    let out = run(&["Here is ", "the answer: ", "9."]);
    assert_eq!(out.reasoning, "");
    assert_eq!(out.content, "Here is the answer: 9.");
}

/// Whitespace between spans is not an answer, so it must not stop later tags
/// being honoured.
#[test]
fn whitespace_between_spans_does_not_end_the_preamble() {
    let out = run(&["<think>a</think>\n\n", "<think>b</think>", "done"]);
    assert_eq!(out.reasoning, "ab");
    assert_eq!(out.content, "done");
}

/// Nothing reaches the answer before the answer starts, whether the whitespace
/// shares a delta with a tag or arrives on its own.
///
/// The accumulator reads a non-empty `content_parts` as the answer having
/// begun, and stops counting toward `AFI_REASONING_ONLY_CHARS` once it has. One
/// newline emitted between two spans would disable that cut for the rest of the
/// stream, which is the loop the cut exists to break.
#[test]
fn preamble_whitespace_never_reaches_the_answer() {
    let mut tags = ReasoningTags::new(true);
    for delta in ["<think>a</think>", "\n", "  ", "\n\n<think>b</think>"] {
        assert_eq!(tags.split(delta).content, "", "on {delta:?}");
    }
    assert_eq!(tags.split("real").content, "real");
}

/// Whitespace the answer itself opens with is not preamble, so it survives.
#[test]
fn whitespace_inside_the_answer_is_kept() {
    let out = run(&["<think>a</think>", "\n\nline one\n\nline two"]);
    assert_eq!(out.content, "\n\nline one\n\nline two");
}

/// Disabled, every byte is the answer - the Anthropic path, where deliberation
/// arrives as thinking blocks and a quoted tag is just text.
#[test]
fn a_disabled_splitter_passes_everything_through() {
    let mut tags = ReasoningTags::new(false);
    let out = tags.split("<think>quoted</think>answer");
    assert_eq!(out.reasoning, "");
    assert_eq!(out.content, "<think>quoted</think>answer");
    assert_eq!(tags.flush(), Split::default());
}

/// `/compress` reads a whole body rather than a stream, and the wrapping is a
/// property of the provider's serialization either way.
#[test]
fn strip_drops_reasoning_from_a_whole_body() {
    assert_eq!(
        strip("<reasoning>notes</reasoning>The summary."),
        "The summary."
    );
    assert_eq!(strip("plain summary"), "plain summary");
    assert_eq!(strip("<think>never closed"), "");
}

/// Multi-byte text must not be cut mid-character while looking for a tag tail.
#[test]
fn a_multibyte_tail_is_not_split_mid_character() {
    let out = run(&["<reasoning>précis</reasoning>résumé"]);
    assert_eq!(out.reasoning, "précis");
    assert_eq!(out.content, "résumé");
}

#[test]
fn hold_partial_keeps_only_a_real_prefix() {
    assert_eq!(hold_partial("abc<", &["<think>"]), ("abc", "<"));
    assert_eq!(hold_partial("abc<thi", &["<think>"]), ("abc", "<thi"));
    assert_eq!(hold_partial("abc<x", &["<think>"]), ("abc<x", ""));
    assert_eq!(hold_partial("plain", &["<think>"]), ("plain", ""));
}

/// An empty delta is common - Bedrock opens every stream with one - and must
/// not be mistaken for the answer beginning.
#[test]
fn empty_deltas_carry_nothing_and_change_nothing() {
    let mut tags = ReasoningTags::new(true);
    assert_eq!(tags.split(""), Split::default());
    let out = tags.split("<reasoning>x</reasoning>");
    assert_eq!(out.reasoning, "x");
    assert!(out.content.is_empty());
}
