//! The system prompt a run sends, resolved once at startup from
//! `--system-prompt-file` / `AFI_SYSTEM_PROMPT_FILE` and
//! `--system-prompt-mode` / `AFI_SYSTEM_PROMPT_MODE`.
//!
//! afi's own prompt is compiled in, so every run has always been told the same
//! things - most of them about launching and waiting on detached shell commands,
//! which a read-only review job resends on every request and can never act on.
//! Writing different instructions into the task prompt file is not the same
//! thing: they arrive as a user message, mixed in with the task.
//!
//! A supplied prompt keeps the text-protocol contract in both modes. afi never
//! learns whether the endpoint it is pointed at parses native tool calls - it
//! sends the schemas and reads both answers - so dropping the contract would
//! leave a model on such an endpoint with no way to call a tool at all, and
//! refusing every replaced run instead would make the mode unusable. See
//! `crate::prompt`.

use std::fs;
use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::prompt;
use crate::util;

/// How a supplied prompt meets the built-in one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptMode {
    /// The supplied text stands in for the built-in guidance, keeping only the
    /// text-protocol contract. The default: replacing is the reason to supply a
    /// prompt at all, and a run that wanted afi's shell guidance can keep it by
    /// asking for `append` rather than by not asking for anything.
    #[default]
    Replace,
    /// The supplied text follows the built-in prompt whole.
    Append,
}

impl PromptMode {
    /// Parse a mode, or `None` when it is not one afi has.
    ///
    /// Unlike `--summary`, an unrecognized value is not shrugged off. The two
    /// modes send materially different instructions, so a typo silently taking
    /// the default would produce a complete, plausible run that was told
    /// something other than what was asked for.
    fn from_value(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "replace" => Some(Self::Replace),
            "append" => Some(Self::Append),
            _ => None,
        }
    }

    /// The name this mode is configured and reported under.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Append => "append",
        }
    }
}

/// The system content every turn of a run sends, and where it came from.
#[derive(Debug, Clone)]
pub struct SystemPrompt {
    text: String,
    /// The file the run was given and how it was combined. `None` is afi's own
    /// prompt, unchanged.
    from: Option<(PromptMode, String)>,
}

impl SystemPrompt {
    /// The system content itself.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How the prompt was built, for the run summary: `builtin`, `replace`, or
    /// `append`.
    #[must_use]
    pub fn mode(&self) -> &'static str {
        self.from
            .as_ref()
            .map_or("builtin", |(mode, _)| mode.as_str())
    }

    /// The file the text came from, as it was written. Absent for `builtin`.
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.from.as_ref().map(|(_, path)| path.as_str())
    }

    /// The message a conversation opens with.
    ///
    /// The one place the shape is written down. Every entry point that starts or
    /// restarts a history - a fresh session, a resume, a one-shot run, `/reset` -
    /// builds it from here, so the next change to the shape is one edit rather
    /// than four that have to agree.
    #[must_use]
    pub fn message(&self) -> Value {
        json!({"role": "system", "content": self.text})
    }
}

/// afi's own prompt, resolved once.
///
/// Exists so `Runtime` has a `'static` answer for a run whose configured prompt
/// failed to resolve, which keeps that fallback to one site instead of one per
/// caller. Reaching it means `refusals` was skipped - see `Runtime::prompt`.
pub(super) fn builtin() -> &'static SystemPrompt {
    static BUILT_IN: LazyLock<SystemPrompt> = LazyLock::new(|| SystemPrompt {
        text: prompt::system(),
        from: None,
    });
    &BUILT_IN
}

/// Resolve the prompt this run sends from an already-merged file and mode.
///
/// Precedence between a flag and its variable is settled by the caller, the way
/// it is for every other setting.
///
/// # Errors
///
/// Returns why a configured prompt cannot be used: an unusable mode, or a file
/// that could not be read, does not exist, or says nothing. Never falls back to
/// the built-in text - a run told to send its own instructions and quietly
/// sending afi's is the failure this whole setting exists to avoid.
pub fn resolve(file: Option<&str>, mode: Option<&str>) -> Result<SystemPrompt, String> {
    // Checked whether or not a file was named. A value afi does not have is a
    // mistake worth hearing about at the moment it is made, rather than on the
    // first run that also supplies a file.
    let mode = match util::nonblank(mode) {
        Some(raw) => PromptMode::from_value(raw).ok_or_else(|| {
            format!("unknown system prompt mode {raw:?} (want replace or append)")
        })?,
        None => PromptMode::default(),
    };
    let Some(path) = util::nonblank(file) else {
        return Ok(builtin().clone());
    };
    let supplied = read(path)?;
    let afi = match mode {
        PromptMode::Replace => prompt::tool_protocol(),
        PromptMode::Append => prompt::system(),
    };
    Ok(SystemPrompt {
        // afi's part first and the supplied text last in both modes, so what the
        // operator wrote reads the same way whichever mode it is sent under.
        text: format!("{afi}\n\n{supplied}"),
        from: Some((mode, path.to_string())),
    })
}

/// The file's contents, or why the run must not start.
///
/// An empty file is refused rather than treated as no file. It is what a
/// truncated write, a wrong path that happens to exist, and an unexpanded
/// template all leave behind, and each of them means the instructions the run
/// was supposed to follow are not there.
fn read(path: &str) -> Result<String, String> {
    let body = fs::read_to_string(path)
        .map_err(|error| format!("can't read the system prompt from {path:?}: {error}"))?;
    let text = body.trim().to_string();
    if text.is_empty() {
        return Err(format!("the system prompt at {path:?} is empty"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
