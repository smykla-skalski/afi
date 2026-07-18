//! Automatic context-compression threshold logic.

use serde_json::Value;

use super::CompressResult;

/// Silently compress when context usage crosses the `autocompress_percent`
/// threshold. Returns `true` if a compression happened.
///
/// `compress_fn` is the closure that does the actual compression (abstracted
/// so this is testable without a live server).
pub fn maybe_autocompress<F>(
    messages: &mut Vec<Value>,
    prompt_tokens: u64,
    autocompress_percent: u32,
    context_window: Option<u64>,
    compress_fn: F,
) -> bool
where
    F: FnOnce(&mut Vec<Value>) -> Option<CompressResult>,
{
    if autocompress_percent == 0 || prompt_tokens == 0 {
        return false;
    }
    let mx = match context_window {
        Some(max_tokens) if max_tokens > 0 => max_tokens,
        _ => return false,
    };
    // Integer math avoids `u64` -> `f64` precision loss: tokens/mx*100 >= pct
    // is equivalent to tokens*100 >= pct*mx (`u128` guards multiplication).
    u128::from(prompt_tokens) * 100 >= u128::from(autocompress_percent) * u128::from(mx)
        && compress_fn(messages).is_some()
}
