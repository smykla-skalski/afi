//! A compiled model-to-context-window table, for the auto-compress threshold.
//!
//! The threshold in `AFI_AUTOCOMPRESS_PERCENT` is a percentage of the context
//! window, so it needs a window to measure against, and no provider afi speaks
//! to volunteers one on the request path. [`crate::pricing`] argues against
//! compiling a table for exactly this reason, and it is right about prices: a
//! stale rate reports a wrong invoice with total confidence. A stale context
//! window is a different kind of wrong. It moves the fold a little early or a
//! little late, both of which a run survives, and an operator can correct it with
//! a setting - see `crate::config::window`, which reads the declared value first
//! and only falls back here.
//!
//! What a table must not do is guess, so a model it has never heard of resolves
//! to `None` and that run does not fold. That is the honest answer rather than a
//! plausible number, and it is why the caller says so on the ui once.
//!
//! Rows are keyed by **provider-native** model id, and the same weights appear
//! under every spelling that serves them, because the servings differ: Z.ai's own
//! `glm-5.2` and `OpenRouter`'s `z-ai/glm-5.2` hold a million tokens while
//! Together's `zai-org/GLM-5.2` holds 262144, and Anthropic serves
//! `claude-sonnet-4-5` with 200000 where a 1M window is a beta opt-in. Nothing
//! here is derived from a family name.
//!
//! ## For whoever vendors the metadata table
//!
//! This is a placeholder for one column of a bigger fetch. models.dev's
//! `api.json` carries `limit.context` beside the token prices, so whoever vendors
//! that table for cost reporting can absorb this file: same fetch, same embed,
//! same staleness story, and this module becomes a lookup into it. Keep the
//! normalization below - it is what lets one row answer for a Bedrock id carrying
//! a Region prefix and an inference-profile version suffix - and keep the rule
//! that an unknown model resolves to `None`.

/// One model id and the context window it is served with, in tokens.
type Row = (&'static str, u64);

/// Windows for the models afi's built-in sources pin, plus the ones commonly
/// configured beside them.
///
/// Sorted by id, which [`tests::the_table_is_sorted_with_no_duplicates`] enforces
/// so the binary search below stays correct. The order is by raw bytes, where
/// `-` sorts before `.` and `.` before `/`, so `zai-org/glm-5` precedes
/// `zai.glm-5`.
///
/// Figures come from models.dev's `api.json` (`limit.context`), except the
/// Anthropic rows, which are Anthropic's own published windows. Where a provider
/// serves a larger window only behind a flag, the row records the default: an
/// under-estimate folds early, which a run survives, and an over-estimate does
/// not fold at all, which is the failure this path exists to prevent.
#[rustfmt::skip]
const WINDOWS: &[Row] = &[
    // Anthropic, as Bedrock spells it. A Region prefix and a version suffix are
    // normalized off before the lookup, so `us.anthropic.claude-opus-5-v1:0`
    // lands on the row below rather than needing one of its own.
    ("anthropic.claude-fable-5",    1_000_000),
    ("anthropic.claude-haiku-4-5",    200_000),
    ("anthropic.claude-opus-4-1",     200_000),
    ("anthropic.claude-opus-4-5",     200_000),
    ("anthropic.claude-opus-4-6",   1_000_000),
    ("anthropic.claude-opus-4-7",   1_000_000),
    ("anthropic.claude-opus-4-8",   1_000_000),
    ("anthropic.claude-opus-5",     1_000_000),
    ("anthropic.claude-sonnet-4-5",   200_000),
    ("anthropic.claude-sonnet-4-6", 1_000_000),
    ("anthropic.claude-sonnet-5",   1_000_000),
    // Anthropic's own API, which the built-in `anthropic` source uses. It
    // defaults to `claude-sonnet-5`.
    ("claude-fable-5",              1_000_000),
    ("claude-haiku-4-5",              200_000),
    ("claude-mythos-5",             1_000_000),
    ("claude-opus-4-1",               200_000),
    ("claude-opus-4-5",               200_000),
    ("claude-opus-4-6",             1_000_000),
    ("claude-opus-4-7",             1_000_000),
    ("claude-opus-4-8",             1_000_000),
    ("claude-opus-5",               1_000_000),
    ("claude-sonnet-4-5",             200_000),
    ("claude-sonnet-4-6",           1_000_000),
    ("claude-sonnet-5",             1_000_000),
    // DeepSeek: Together's spelling, then its own API, then Bedrock's, then
    // OpenRouter's.
    ("deepseek-ai/deepseek-v3",        131_072),
    ("deepseek-ai/deepseek-v3-1",      131_072),
    ("deepseek-chat",               1_000_000),
    ("deepseek-reasoner",           1_000_000),
    ("deepseek.r1",                   128_000),
    ("deepseek.v3",                   163_840),
    ("deepseek.v3.2",                 163_840),
    ("deepseek/deepseek-chat",        163_840),
    ("deepseek/deepseek-v3.2",        163_840),
    // Z.ai's own API, the `zai` source of `sources.example.env`.
    ("glm-4.5",                       131_072),
    ("glm-4.5-air",                   131_072),
    ("glm-4.6",                       204_800),
    ("glm-4.7",                       204_800),
    ("glm-4.7-flash",                 200_000),
    ("glm-5",                         204_800),
    ("glm-5.1",                       200_000),
    ("glm-5.2",                     1_000_000),
    // OpenAI's own API.
    ("gpt-5",                         400_000),
    ("gpt-5-mini",                    400_000),
    ("gpt-5.1",                       400_000),
    ("gpt-5.2",                       400_000),
    ("gpt-5.3-codex",                 400_000),
    ("gpt-5.4",                     1_050_000),
    // Moonshot's own API, then Together's spelling.
    ("kimi-k2.5",                     262_144),
    ("kimi-k2.6",                     262_144),
    ("kimi-k3",                     1_048_576),
    ("moonshotai/kimi-k2.5",          262_144),
    ("moonshotai/kimi-k2.6",          262_144),
    ("moonshotai/kimi-k3",          1_048_576),
    // The open-weight models Bedrock serves, and OpenRouter's OpenAI ids.
    ("openai.gpt-oss-120b",           128_000),
    ("openai.gpt-oss-20b",            128_000),
    ("openai/gpt-5.2",                400_000),
    ("qwen.qwen3-235b-a22b-2507",     262_144),
    ("qwen.qwen3-coder-480b-a35b",    131_072),
    ("qwen/qwen3-coder",              262_144),
    // OpenRouter, whose ids are `org/model`. The built-in `openrouter` source
    // defaults to `z-ai/glm-5.2`.
    ("z-ai/glm-4.6",                  204_800),
    ("z-ai/glm-4.7",                  204_800),
    ("z-ai/glm-5",                    204_800),
    ("z-ai/glm-5.1",                  204_800),
    ("z-ai/glm-5.2",                1_048_576),
    // Together, whose ids are `org/Model`. The built-in `together` source
    // defaults to `zai-org/GLM-5.2`.
    ("zai-org/glm-5",                 202_752),
    ("zai-org/glm-5.1",               202_752),
    ("zai-org/glm-5.2",               262_144),
    // Bedrock's GLM ids. The built-in `bedrock` source defaults to `zai.glm-5`.
    ("zai.glm-4.7",                   204_800),
    ("zai.glm-4.7-flash",             200_000),
    ("zai.glm-5",                     202_752),
];

/// The Region and cross-Region prefixes Bedrock puts on an inference-profile id.
/// Every one of them serves the same weights with the same window, so they are
/// stripped rather than given rows of their own.
const REGION_PREFIXES: [&str; 7] = ["us.", "eu.", "apac.", "au.", "jp.", "global.", "us-gov."];

/// The context window `model` is served with, or `None` when the table has never
/// heard of it.
///
/// Matching is exact on the normalized id, deliberately. No family or
/// longest-prefix rule would be safe: `glm-5` is a prefix of `glm-5.2`, and the
/// two differ by a factor of five, so a prefix match would silently answer a 1M
/// model with a 204800 window - or the reverse, which is the direction that never
/// folds. An id this cannot place is answered with `None`.
#[must_use]
pub fn context_window_for(model: &str) -> Option<u64> {
    let normalized = normalize(model);
    lookup(&normalized).or_else(|| lookup(without_date_stamp(&normalized)))
}

/// Fold an id into its table key: case and surrounding space dropped, then the
/// two decorations Bedrock adds - a Region prefix and a version suffix - taken
/// off, so `us.anthropic.claude-opus-5-v1:0` and `anthropic.claude-opus-5` are
/// one row rather than a dozen.
fn normalize(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let body = REGION_PREFIXES
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(lower.as_str());
    without_version_suffix(body).to_string()
}

/// Drop a trailing Bedrock model-version suffix. Bedrock writes it two ways -
/// `-v1:0` on a versioned model id and a bare `-v1` on a cross-Region profile -
/// so both halves come off independently.
///
/// Only the `-v<n>` form is stripped. `deepseek.v3` keeps its `v3`, because there
/// the version *is* the model rather than a revision of one, and the leading `.`
/// is what tells the two apart.
fn without_version_suffix(id: &str) -> &str {
    let body = match id.rsplit_once(':') {
        Some((head, revision)) if is_digits(revision) => head,
        _ => id,
    };
    match body.rsplit_once("-v") {
        Some((before, version)) if is_digits(version) => before,
        _ => body,
    }
}

/// The same id without a trailing `-YYYYMMDD` release stamp, so one row answers
/// for `claude-haiku-4-5` and `claude-haiku-4-5-20251001` both. Returns the id
/// unchanged when it carries no stamp.
fn without_date_stamp(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((head, stamp)) if stamp.len() == 8 && is_digits(stamp) => head,
        _ => id,
    }
}

/// Whether `part` is a non-empty run of ASCII digits.
fn is_digits(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
}

fn lookup(key: &str) -> Option<u64> {
    WINDOWS
        .binary_search_by_key(&key, |(id, _)| *id)
        .ok()
        .map(|index| WINDOWS[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_with_no_duplicates() {
        // The lookup is a binary search, so an out-of-order row is not a tidiness
        // problem: it makes that row unfindable, and the model it names silently
        // stops folding. A duplicate is the same failure from the other side -
        // which of the two answers would depend on where the search landed.
        for pair in WINDOWS.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{:?} must sort before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    #[test]
    fn a_native_id_resolves() {
        assert_eq!(context_window_for("claude-sonnet-5"), Some(1_000_000));
        assert_eq!(context_window_for("zai.glm-5"), Some(202_752));
    }

    #[test]
    fn case_and_surrounding_space_do_not_matter() {
        // Together spells its ids with capitals, and a value read from a config
        // file arrives with whatever whitespace was around it.
        assert_eq!(context_window_for("  zai-org/GLM-5.2\n"), Some(262_144));
    }

    #[test]
    fn one_spelling_does_not_answer_for_another() {
        // The three GLM 5.2 servings really are different sizes. A family rule
        // would have to pick one of them and be wrong twice.
        assert_eq!(context_window_for("glm-5.2"), Some(1_000_000));
        assert_eq!(context_window_for("z-ai/glm-5.2"), Some(1_048_576));
        assert_eq!(context_window_for("zai-org/glm-5.2"), Some(262_144));
    }

    #[test]
    fn a_prefix_of_a_known_id_is_not_a_match() {
        // `glm-5` is a prefix of `glm-5.2` and holds a fifth as much, so the
        // lookup must stay exact rather than reaching for the longest prefix.
        assert_eq!(context_window_for("glm-5"), Some(204_800));
        assert_eq!(context_window_for("glm-5.9"), None);
    }

    #[test]
    fn a_bedrock_region_prefix_and_version_suffix_are_stripped() {
        for id in [
            "us.anthropic.claude-opus-5",
            "eu.anthropic.claude-opus-5",
            "global.anthropic.claude-opus-5",
            "anthropic.claude-opus-5-v1:0",
            "us.anthropic.claude-opus-5-v1",
        ] {
            assert_eq!(context_window_for(id), Some(1_000_000), "{id}");
        }
    }

    #[test]
    fn a_dated_release_resolves_to_its_family_row() {
        assert_eq!(
            context_window_for("claude-haiku-4-5-20251001"),
            Some(200_000)
        );
        assert_eq!(
            context_window_for("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
            Some(200_000)
        );
    }

    #[test]
    fn a_version_that_is_the_model_is_left_alone() {
        // `deepseek.v3` would resolve to `deepseek` if the `-v<n>` rule were a
        // `v<n>` rule.
        assert_eq!(context_window_for("deepseek.v3-v1:0"), Some(163_840));
        assert_eq!(context_window_for("deepseek.v3"), Some(163_840));
    }

    #[test]
    fn an_unknown_model_has_no_window() {
        // The llama.cpp case: a path to a local gguf tells afi nothing about how
        // much context the server was started with.
        assert_eq!(context_window_for("qwen3-coder-30b-q4_k_m"), None);
        assert_eq!(context_window_for(""), None);
        assert_eq!(context_window_for("local-model"), None);
    }
}
