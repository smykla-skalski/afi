//! Reasoning a model wrote into `content` instead of into its own field.
//!
//! An `OpenAI`-compatible endpoint is supposed to stream deliberation in
//! `reasoning_content`, which afi renders on its own channel and keeps out of
//! the answer. Several endpoints do not: they wrap it in `<think>` or
//! `<reasoning>` and put it in `content`, where it reads as the model's reply
//! and lands in `summary.answer`. Bedrock's open-weight surface does this for
//! `openai.gpt-oss-*`, `moonshot.kimi-k2-thinking`, and `minimax.minimax-m2.5`;
//! llama.cpp and vLLM do it for any reasoning model started without a reasoning
//! parser.
//!
//! This lifts those spans back out and hands them to the caller separately, so
//! the rest of afi sees what the field-based providers already produce. Nothing
//! is discarded - text inside the tags becomes reasoning, text outside stays
//! content.
//!
//! # Why a state machine
//!
//! Two shapes have to work at once. Bedrock wraps *every delta* in its own
//! matched pair, so a machine that opened once and closed once would treat the
//! second delta's tag as literal text. A self-hosted server emits one span
//! across many deltas, so a machine that looked only within a delta would never
//! see the close, and a tag can be cut anywhere - `<reas` ending one delta and
//! `oning>` starting the next.
//!
//! # Why the tags are only honoured before the answer starts
//!
//! `<think>` is ordinary text in a reply about prompting, or about this module.
//! Reasoning always precedes the answer, so once real content has been emitted
//! the tags are left alone and a model quoting one is quoted back faithfully.

use std::mem::take;

/// The tag pairs recognised. Order is not load-bearing: [`find_open`] picks by
/// position and [`hold_partial`] asks `any`.
const PAIRS: [(&str, &str); 2] = [("<reasoning>", "</reasoning>"), ("<think>", "</think>")];

/// Just the opening tags, for the scan that runs when none of them is present.
const OPENS: [&str; 2] = [PAIRS[0].0, PAIRS[1].0];

/// One delta's content, divided by where it belongs.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Split {
    pub reasoning: String,
    pub content: String,
}

/// Fold state for one SSE stream. One per streamed response: a turn that calls
/// tools opens a fresh stream per request, and each starts with its own
/// reasoning.
#[derive(Debug)]
pub(crate) struct ReasoningTags {
    /// Held back because it may be the front of a tag that the next delta
    /// finishes. Released by [`ReasoningTags::flush`] if the stream ends first.
    partial: String,
    /// Index into [`PAIRS`] of the span currently open, if any.
    open: Option<usize>,
    /// Answer content has been emitted, so every later tag is literal.
    answered: bool,
    /// Whether to look at all. Anthropic reports deliberation as `thinking`
    /// blocks and never wraps it in tags, so there is nothing to gain there and
    /// a reply that opens by quoting one would be moved off the answer for no
    /// reason.
    enabled: bool,
}

impl ReasoningTags {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            partial: String::new(),
            open: None,
            answered: false,
            enabled,
        }
    }

    /// Divide one delta's content, carrying any cut tag into the next call.
    pub(crate) fn split(&mut self, text: &str) -> Split {
        if !self.enabled {
            return Split {
                reasoning: String::new(),
                content: text.to_string(),
            };
        }
        let mut out = Split::default();
        let joined = take(&mut self.partial) + text;
        let mut rest: &str = &joined;
        loop {
            // Reached only once the answer has begun, and `take_preamble` is the
            // only thing that sets that - so a span is never open here.
            if self.answered {
                out.content.push_str(rest);
                return out;
            }
            let advanced = match self.open {
                Some(index) => self.take_open_span(rest, index, &mut out),
                None => self.take_preamble(rest, &mut out),
            };
            let Some(after) = advanced else { return out };
            rest = after;
        }
    }

    /// Release whatever was held back when the stream ends mid-tag.
    ///
    /// An unterminated span is still reasoning: a model cut off deliberating
    /// produced no answer, and calling it one would put its notes in
    /// `summary.answer` - the failure this module exists to prevent. A dangling
    /// tag prefix is text the model sent, so it goes back to the answer.
    pub(crate) fn flush(&mut self) -> Split {
        let held = take(&mut self.partial);
        let mut out = Split::default();
        if self.open.is_some() {
            out.reasoning = held;
        } else {
            out.content = held;
        }
        out
    }

    /// Consume as much of an open reasoning span as `rest` holds, returning what
    /// follows its close.
    fn take_open_span<'a>(
        &mut self,
        rest: &'a str,
        index: usize,
        out: &mut Split,
    ) -> Option<&'a str> {
        let close = PAIRS[index].1;
        if let Some(at) = rest.find(close) {
            out.reasoning.push_str(&rest[..at]);
            self.open = None;
            return Some(&rest[at + close.len()..]);
        }
        let (emit, held) = hold_partial(rest, &[close]);
        out.reasoning.push_str(emit);
        self.partial = held.to_string();
        None
    }

    /// Consume text before the answer has started, returning what follows an
    /// opening tag.
    ///
    /// Whitespace here is dropped rather than emitted. It sits between spans or
    /// in front of the first one, so it is not the answer - and emitting it
    /// would put a part in `content_parts`, which the reasoning-only cut reads
    /// as the answer having started. One newline between two spans would
    /// otherwise disable that cut for the rest of the stream.
    fn take_preamble<'a>(&mut self, rest: &'a str, out: &mut Split) -> Option<&'a str> {
        if let Some((at, index)) = find_open(rest) {
            if rest[..at].trim().is_empty() {
                self.open = Some(index);
                return Some(&rest[at + PAIRS[index].0.len()..]);
            }
        } else {
            let (emit, held) = hold_partial(rest, &OPENS);
            if emit.trim().is_empty() {
                self.partial = held.to_string();
                return None;
            }
        }
        // Content came first, so this tag - and every later one - is the reply.
        self.answered = true;
        out.content.push_str(rest);
        None
    }
}

/// Divide one complete, non-streamed body and keep only the answer.
///
/// The wrapping is a property of how the provider serializes a reply, not of
/// streaming, so a body read in one piece carries it too.
pub(crate) fn strip(text: &str) -> String {
    let mut tags = ReasoningTags::new(true);
    let mut out = tags.split(text);
    out.content.push_str(&tags.flush().content);
    out.content
}

/// The earliest opening tag in `text`, with its index into [`PAIRS`].
fn find_open(text: &str) -> Option<(usize, usize)> {
    PAIRS
        .iter()
        .enumerate()
        .filter_map(|(index, (open, _))| text.find(open).map(|at| (at, index)))
        .min_by_key(|(at, _)| *at)
}

/// Divide `text` into what can be emitted now and what must wait, holding back
/// any tail that is a proper prefix of one of `tags`.
///
/// Without this a tag cut across two deltas would be streamed to the user as
/// text and never recognised, since neither half contains it.
///
/// Every tag is ASCII and starts with `<`, and none contains a second one, so
/// only a tail starting at the last `<` can be the front of one. Callers must
/// have established that no complete tag is present.
fn hold_partial<'a>(text: &'a str, tags: &[&str]) -> (&'a str, &'a str) {
    if let Some(at) = text.rfind('<')
        && tags.iter().any(|tag| tag.starts_with(&text[at..]))
    {
        return (&text[..at], &text[at..]);
    }
    (text, "")
}

#[cfg(test)]
mod tests;
