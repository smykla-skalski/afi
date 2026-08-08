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

/// The tag pairs recognised, longest first so `find_open` cannot match a prefix
/// of one inside another.
const PAIRS: [(&str, &str); 2] = [("<reasoning>", "</reasoning>"), ("<think>", "</think>")];

/// One delta's content, divided by where it belongs.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Split {
    pub reasoning: String,
    pub content: String,
}

impl Split {
    /// Whether this delta carried nothing, so a caller can skip it entirely.
    pub(crate) fn is_empty(&self) -> bool {
        self.reasoning.is_empty() && self.content.is_empty()
    }
}

/// Fold state for one SSE stream. One per streamed response: a turn that calls
/// tools opens a fresh stream per request, and each starts with its own
/// reasoning.
#[derive(Debug, Default)]
pub(crate) struct ReasoningTags {
    /// Held back because it may be the front of a tag that the next delta
    /// finishes. Released by [`ReasoningTags::flush`] if the stream ends first.
    partial: String,
    /// Index into [`PAIRS`] of the span currently open, if any.
    open: Option<usize>,
    /// Answer content has been emitted, so every later tag is literal.
    answered: bool,
}

impl ReasoningTags {
    /// Divide one delta's content, carrying any cut tag into the next call.
    pub(crate) fn split(&mut self, text: &str) -> Split {
        let mut out = Split::default();
        let mut rest = take(&mut self.partial);
        rest.push_str(text);
        loop {
            if let Some(index) = self.open {
                if !self.take_open_span(&rest, index, &mut out) {
                    return out;
                }
                rest = take(&mut self.partial);
                continue;
            }
            if self.answered {
                out.content.push_str(&rest);
                return out;
            }
            if !self.take_preamble(&rest, &mut out) {
                return out;
            }
            rest = take(&mut self.partial);
        }
    }

    /// Release whatever was held back when the stream ends mid-tag.
    ///
    /// An unterminated span is still reasoning: a model cut off deliberating
    /// produced no answer, and calling it one would put its notes in
    /// `summary.answer` - the failure this module exists to prevent.
    pub(crate) fn flush(&mut self) -> Split {
        let held = take(&mut self.partial);
        if held.is_empty() {
            return Split::default();
        }
        if self.open.is_some() {
            return Split {
                reasoning: held,
                content: String::new(),
            };
        }
        Split {
            reasoning: String::new(),
            content: held,
        }
    }

    /// Consume as much of an open reasoning span as `rest` holds. Returns
    /// whether the span closed, leaving the text after it in `self.partial`.
    fn take_open_span(&mut self, rest: &str, index: usize, out: &mut Split) -> bool {
        let close = PAIRS[index].1;
        if let Some(at) = rest.find(close) {
            out.reasoning.push_str(&rest[..at]);
            self.partial = rest[at + close.len()..].to_string();
            self.open = None;
            return true;
        }
        let (emit, held) = hold_partial(rest, &[close]);
        out.reasoning.push_str(emit);
        self.partial = held.to_string();
        false
    }

    /// Consume text before the answer has started. Returns whether a tag opened,
    /// leaving the text after it in `self.partial`.
    fn take_preamble(&mut self, rest: &str, out: &mut Split) -> bool {
        if let Some((at, index)) = find_open(rest) {
            let before = &rest[..at];
            if before.trim().is_empty() {
                out.content.push_str(before);
                self.partial = rest[at + PAIRS[index].0.len()..].to_string();
                self.open = Some(index);
                return true;
            }
            // Content came first, so this tag is part of the reply.
            self.answered = true;
            out.content.push_str(rest);
            return false;
        }
        let opens: Vec<&str> = PAIRS.iter().map(|pair| pair.0).collect();
        let (emit, held) = hold_partial(rest, &opens);
        if emit.trim().is_empty() {
            out.content.push_str(emit);
            self.partial = held.to_string();
            return false;
        }
        // Real content with no tag in front of it: nothing later can be one.
        self.answered = true;
        out.content.push_str(rest);
        false
    }
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
fn hold_partial<'a>(text: &'a str, tags: &[&str]) -> (&'a str, &'a str) {
    let longest = tags.iter().map(|tag| tag.len()).max().unwrap_or(0);
    let start = text.len().saturating_sub(longest.saturating_sub(1));
    for at in start..text.len() {
        if !text.is_char_boundary(at) {
            continue;
        }
        let tail = &text[at..];
        if tags.iter().any(|tag| tag.starts_with(tail)) {
            return (&text[..at], tail);
        }
    }
    (text, "")
}

#[cfg(test)]
mod tests;
