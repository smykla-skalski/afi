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

use std::collections::HashMap;
use std::fs;
use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::prompt;
use crate::util;

use super::args::ParsedArgs;
use super::instructions::{self, Instructions};

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
    /// The values [`Self::from_value`] accepts, for a caller that has to name
    /// them in a refusal - the config file. One list, so the file cannot come to
    /// accept a mode the variable does not.
    pub(super) const NAMES: [&str; 2] = ["replace", "append"];

    /// Parse a mode, or `None` when it is not one afi has.
    ///
    /// Unlike `--summary`, an unrecognized value is not shrugged off. The two
    /// modes send materially different instructions, so a typo silently taking
    /// the default would produce a complete, plausible run that was told
    /// something other than what was asked for.
    pub(super) fn from_value(raw: &str) -> Option<Self> {
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
    /// The project instructions whose text is already in `text`, kept as the value
    /// the walk produced rather than exploded into fields.
    ///
    /// Whole, because the exploded form lost a field: `walked` was assigned after an
    /// early return that fired whenever the walk found no file, so a repository with
    /// rules only in a subtree silently got neither half of the feature. A value that
    /// is moved in one piece has nothing to drop.
    instructions: Instructions,
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

    /// The project instructions this prompt carries, for the rest of `config` -
    /// [`instructions::nested`] asks whether a walk found them and which files it
    /// already sent.
    pub(super) fn instructions(&self) -> &Instructions {
        &self.instructions
    }

    /// The instruction files this prompt carries - see [`Instructions::files`].
    #[must_use]
    pub(crate) fn instruction_files(&self) -> Vec<(String, usize)> {
        self.instructions.files()
    }

    /// The same prompt with a project's own instructions after it.
    ///
    /// Last, and after a supplied prompt rather than before it, so the seam
    /// between afi's part and the operator's is the one the two modes already
    /// agree on. What the repository said does not become the final word by
    /// sitting there - the block says so itself, which is the only form of
    /// precedence a model can act on.
    ///
    /// A walk that found nothing still lands here, text or no text: it is the run's
    /// answer to "read the tree", and the subtree half reads from it later.
    pub(super) fn with(mut self, loaded: Instructions) -> Self {
        if let Some(block) = loaded.block() {
            self.text.push_str("\n\n");
            self.text.push_str(&block);
        }
        self.instructions = loaded;
        self
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
        instructions: Instructions::default(),
    });
    &BUILT_IN
}

/// The whole system prompt this run sends, from every flag and variable that has
/// a say in it.
///
/// One result rather than two, so no path exists where a run sends a prompt
/// missing the project rules it was told to follow. The supplied file resolves
/// first because it is the more explicit of the two and only one refusal is
/// reported. A flag beats its variable, as everywhere else.
///
/// The instruction walk reads the process's own working directory, and only when
/// the setting asks for it. That is what keeps a `Runtime` built from a caller's
/// own env from touching a tree nobody named - see `Runtime::resolve_env`.
///
/// # Errors
///
/// Whatever [`resolve`] or [`instructions::resolve`] refused, either of which is
/// the run being told to follow instructions it cannot.
pub(super) fn for_run(
    parsed: &ParsedArgs,
    env: &HashMap<String, String>,
) -> Result<SystemPrompt, String> {
    let prompt = resolve(
        setting(
            parsed.system_prompt_file.as_ref(),
            env.get("AFI_SYSTEM_PROMPT_FILE"),
        ),
        setting(
            parsed.system_prompt_mode.as_ref(),
            env.get("AFI_SYSTEM_PROMPT_MODE"),
        ),
    )?;
    let loaded = instructions::resolve(
        setting(parsed.instructions.as_ref(), env.get("AFI_INSTRUCTIONS")),
        None,
        env,
    )?;
    Ok(prompt.with(loaded))
}

/// The flag if it was given, else the variable. The one rule, applied three times.
fn setting<'a>(flag: Option<&'a String>, variable: Option<&'a String>) -> Option<&'a str> {
    flag.or(variable).map(String::as_str)
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
        instructions: Instructions::default(),
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
    let text = body.trim_matches(unreadable).to_string();
    if text.is_empty() {
        return Err(format!("the system prompt at {path:?} is empty"));
    }
    Ok(text)
}

/// Whether `c` is a character the model cannot act on.
///
/// Wider than `char::is_whitespace`, which follows the Unicode `White_Space`
/// property and therefore keeps a byte-order mark, a zero-width space, and a
/// word joiner. A file holding only those is as empty as one holding only
/// spaces, and it is what an editor writing a BOM into an otherwise-truncated
/// file leaves behind - but `trim` would pass it, and the run would go out
/// reporting instructions the model never received. Trimming rather than
/// rejecting also drops the BOM a Windows editor puts in front of a real prompt,
/// which is noise the model would otherwise be sent.
///
/// Shared with [`super::instructions`], which trims the same way: a repository's
/// `AGENTS.md` holding nothing but a byte-order mark is as empty as a supplied
/// prompt holding one.
pub(super) fn unreadable(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c,
            '\u{00ad}'               // soft hyphen
            | '\u{200b}'..='\u{200f}' // zero-width space through RTL mark
            | '\u{2028}'..='\u{202e}' // line/paragraph separators, bidi embedding
            | '\u{2060}'..='\u{2064}' // word joiner through invisible plus
            | '\u{feff}'              // byte-order mark
        )
}

#[cfg(test)]
mod tests;
