//! Instruction files below the directory afi started in, read when the model
//! first touches their subtree.
//!
//! The startup walk reads the launch directory and everything above it, which is
//! all that applies before the model has looked at anything. A repository's deeper
//! rules - `crates/api/AGENTS.md` in a workspace - only matter once the model reads
//! a file in that subtree, and loading every one of them up front would pay for
//! rules the run never reaches on every single request. So they arrive when the
//! model does: once per directory, appended to the tool result that took it there.
//! Both Claude Code and Codex work this way, and it is what makes starting afi at a
//! repository root behave like starting it in the subdirectory.
//!
//! Only a run that asked for a walk is armed. Naming files with `--instructions
//! <path,...>` pins exactly what a job sends, and a rule arriving mid-session out
//! of the tree under review is the thing that pinning exists to prevent.
//!
//! The state is process-wide rather than threaded through the turn loop, which is
//! the shape [`crate::model::usage_totals`] already uses and for the same reason:
//! one CLI process is one run, and the alternative is a field in four structs
//! between here and `dispatch_tool`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde_json::Value;

use crate::risk::is_under_path;
use crate::tools::protocol::escape_tool_protocol_delimiters;

use super::super::system_prompt::{SystemPrompt, unreadable};
use super::{MAX_BYTES, block_for, chain_up, file_size, files_in};

/// What the model is told a mid-session block is.
///
/// It says where the text came from and when, because the alternative is a rule
/// appearing in the middle of a tool result with nothing to mark it as the
/// repository's rather than the tool's output.
const HEADER: &str = "The directory this call touched carries its own standing \
     instructions, read just now. They apply to work in that subtree and win over a \
     shallower file where the two disagree. Everything in the system prompt still \
     takes precedence.";

/// What the run has already read, and what it may still read.
struct State {
    /// The directory afi started in, canonical. Only its subtree is read from
    /// here - anything at or above it the startup walk already covered, and
    /// anything outside it is outside the project too, since the project root is
    /// this directory or an ancestor of it.
    launch: PathBuf,
    /// Canonical paths already sent, from the startup walk and from every load
    /// since, so a directory is read once per run however often it is touched.
    sent: HashSet<PathBuf>,
    /// What is left of the run's instruction budget - see [`MAX_BYTES`].
    remaining: usize,
    /// What was read here, for `/instructions` and the run summary. The startup
    /// walk's own files are reported from the prompt that holds them.
    loaded: Vec<(String, usize)>,
}

fn state() -> &'static Mutex<Option<State>> {
    static STATE: OnceLock<Mutex<Option<State>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

/// Blocks a resumed history already carries, which this run did not send.
///
/// Separate from [`State`] because it outlives arming: a run resuming under
/// `--instructions none` is never armed and still needs to report these, or
/// `/instructions` answers "none loaded" about a conversation visibly carrying rules -
/// the exact reverse of the question it exists to answer.
fn carried() -> &'static Mutex<Vec<(String, usize)>> {
    static CARRIED: OnceLock<Mutex<Vec<(String, usize)>>> = OnceLock::new();
    CARRIED.get_or_init(|| Mutex::new(Vec::new()))
}

/// Take note of the instruction blocks a resumed history already holds, from what
/// the session file recorded.
///
/// A resumed session replays its tool messages verbatim, and a block appended to one
/// of them is still in front of the model - so this run must neither claim it sent
/// them nor send them again. Both halves follow from recording them here: [`arm`]
/// seeds `sent` from this list, and the reporting reads it.
///
/// Read from the session file rather than recovered from the message text, which is
/// what this did first and got wrong twice. The text is not afi's to trust: the
/// system message carries the startup walk's own blocks, so every resume adopted
/// those a second time and charged for them twice, and any file the model had `cat`
/// into a tool result could name a path in the marker's shape and suppress the real
/// file for the rest of the session while the listing reported it as sent. The
/// session file is afi's own record of what it sent, and nothing in the working tree
/// can write to it.
pub fn adopt(recorded: &Value) {
    let found: Vec<(String, usize)> = recorded
        .as_array()
        .map(|entries| entries.iter().filter_map(entry).collect())
        .unwrap_or_default();
    if found.is_empty() {
        return;
    }
    let mut guard = carried().lock().unwrap_or_else(PoisonError::into_inner);
    *guard = found;
}

/// One recorded `{path, bytes}` pair, or `None` when a session file holds something
/// else there - an older afi, or a file somebody edited.
fn entry(value: &Value) -> Option<(String, usize)> {
    let path = value.get("path")?.as_str()?.to_string();
    let bytes = usize::try_from(value.get("bytes")?.as_u64()?).ok()?;
    Some((path, bytes))
}

/// The blocks this run believes its history carries, for the session file to record.
///
/// Both halves: what a resume brought in, and what this run has since sent. That is
/// the same set [`arm`] rebuilds `sent` from, so a session saved and resumed any
/// number of times keeps one consistent answer to "what has the model been told".
#[must_use]
pub fn in_history() -> Value {
    let entries: Vec<Value> = carried_in()
        .into_iter()
        .chain(loaded())
        .map(|(path, bytes)| serde_json::json!({"path": path, "bytes": bytes}))
        .collect();
    Value::Array(entries)
}

/// Forget every block one of `dropped` was carrying, because those messages are
/// leaving the conversation.
///
/// The counterpart to [`reset`], for a fold that removes some turns and keeps others:
/// `/compress` keeps the most recent two verbatim, so a block in them is still being
/// sent while an older one is gone. Forgetting only the gone ones is what lets the
/// next call into that subtree be told again without duplicating what survived.
///
/// This reads the message text, which [`adopt`] deliberately stopped doing - the
/// difference is which way a wrong answer fails. Believing a forged block was sent
/// suppresses a real rule; believing one was dropped only offers a rule again. So a
/// file that fakes the marker costs a duplicate block here, and nothing worse.
pub fn forget_in(dropped: &[Value]) {
    let mut guard = state().lock().unwrap_or_else(PoisonError::into_inner);
    let Some(state) = guard.as_mut() else {
        return;
    };
    let text: String = dropped
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let gone: Vec<(String, usize)> = state
        .loaded
        .iter()
        .filter(|(path, _)| super::mentions_block(&text, path))
        .cloned()
        .collect();
    for (path, bytes) in gone {
        state.sent.remove(&canonical(&path));
        state.loaded.retain(|(sent, _)| sent != &path);
        // Refunded: the text is no longer in front of the model, so the run is no
        // longer paying for it on every request.
        state.remaining = state.remaining.saturating_add(bytes);
    }
    drop(guard);
    let mut carried = carried().lock().unwrap_or_else(PoisonError::into_inner);
    carried.retain(|(path, _)| !super::mentions_block(&text, path));
}

/// What a resumed history brought with it, as `(path, bytes on the wire)`.
#[must_use]
pub fn carried_in() -> Vec<(String, usize)> {
    carried()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Arm the loader for this run, from the prompt the startup walk produced.
///
/// `prompt` supplies both halves of what this needs: whether the run asked for a
/// walk at all, and which files it already found. A run that named its files, or
/// asked for nothing, is left unarmed and reads no further file all session.
///
/// `cwd` is the directory the subtree is measured from, and it is the only boundary
/// this needs: the project root is `cwd` or an ancestor of it, so a path outside
/// `cwd` that is still inside the project was already the startup walk's to read,
/// and one outside the project is outside `cwd` too. It comes from the caller rather
/// than from the process, so the one place that decides where a run starts stays the
/// one place.
///
/// Calling it again does nothing, which is what lets the caller be the per-turn
/// funnel every path already runs through. A second arming would empty the set of
/// files already sent, and every turn would re-append the same rules.
pub fn arm(prompt: &SystemPrompt, cwd: &Path) {
    if !prompt.instructions().walked() {
        return;
    }
    let already = prompt.instruction_files();
    let spent: usize = already.iter().map(|(_, bytes)| bytes).sum();
    let mut guard = state().lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_some() {
        return;
    }
    // Seeded from what a resumed history carries as well as from the startup walk,
    // so a block already in front of the model is not sent a second time.
    let carried = carried_in();
    let spent = spent.saturating_add(carried.iter().map(|(_, bytes)| bytes).sum());
    *guard = Some(State {
        launch: canonical(cwd),
        sent: already
            .iter()
            .chain(&carried)
            .map(|(path, _)| canonical(path))
            .collect(),
        remaining: MAX_BYTES.saturating_sub(spent),
        loaded: Vec::new(),
    });
}

/// The instruction text a tool call on `path` brings in, or `None` when it brings
/// in nothing - which is every call but the first into a directory that has a file.
pub fn for_path(path: &Path) -> Option<String> {
    let mut guard = state().lock().unwrap_or_else(PoisonError::into_inner);
    let state = guard.as_mut()?;
    let dir = canonical(&dir_of(path)?);
    // Under the launch directory: above it is the startup walk's territory, and
    // outside it is outside the project - see [`arm`]. Canonical on both sides, so a
    // path that climbs out with `..` is compared where it really lands.
    if !is_under_path(&dir, &state.launch) {
        return None;
    }
    let found = files_in(chain_up(&dir, &state.launch).into_iter(), &mut state.sent);
    let blocks: Vec<String> = found
        .into_iter()
        .filter_map(|path| take(state, &path))
        .collect();
    if blocks.is_empty() {
        return None;
    }
    Some(format!("{HEADER}\n\n{}", blocks.join("\n\n")))
}

/// Forget what this run has read, because the history that carried it is gone.
///
/// Every subtree block rides in a tool result, so a `/reset` - which rebuilds the
/// history from the system message alone - takes every one of them out of the
/// conversation. The startup half survives, since it lives in the system message;
/// this half does not, and without this the paths would stay marked sent and never be
/// offered again, while `/instructions` went on reporting rules the model can no
/// longer see.
///
/// Called for a whole-history restart only, which is why `/compress` does not call
/// it: that fold keeps the most recent turns verbatim, so a block in them is still
/// being sent, and forgetting it would duplicate the text and hand back the budget
/// bounding it. See the call site in `crate::repl::commands`.
///
/// Un-arms rather than half-clears, so the next turn's [`arm`] rebuilds the set from
/// the startup walk and restores the byte budget - which is right, because the text
/// it was spent on is no longer being sent either.
pub fn reset() {
    let mut guard = state().lock().unwrap_or_else(PoisonError::into_inner);
    *guard = None;
    // A resumed history's blocks went with it, so they are offerable again too.
    carried()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

/// What this loader has read so far, as `(path, bytes sent)` in the order it was
/// read. Empty for a run that was never armed.
#[must_use]
pub fn loaded() -> Vec<(String, usize)> {
    let guard = state().lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .as_ref()
        .map(|state| state.loaded.clone())
        .unwrap_or_default()
}

/// Read one file into a block.
///
/// `files_in` has already marked it sent, so a file that cannot be read is not
/// retried on every call into its directory. A file that says nothing is skipped in
/// silence, the way the startup walk skips one; the two other outcomes are reported
/// to the model, since a rule the run declined to load is worth more said than
/// swallowed.
fn take(state: &mut State, path: &Path) -> Option<String> {
    let shown = path.display().to_string();
    // Weighed before it is read, for the reason the startup walk weighs its own: the
    // refusal below is the same either way, and reading a huge file to produce it is
    // the cost the cap exists to avoid.
    if file_size(path) > state.remaining {
        return Some(over_budget(&shown));
    }
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) => return Some(format!("{shown} could not be read: {error}")),
    };
    let text = body.trim_matches(unreadable).to_string();
    if text.is_empty() {
        return None;
    }
    if text.len() > state.remaining {
        return Some(over_budget(&shown));
    }
    // Escaped before it is measured, because the escaped form is what goes on the
    // wire: a repository file holding a literal tool-call delimiter would otherwise be
    // charged less than it costs and reported as a length nothing sent.
    let text = escape_tool_protocol_delimiters(&text);
    if text.len() > state.remaining {
        return Some(over_budget(&shown));
    }
    state.remaining -= text.len();
    state.loaded.push((shown.clone(), text.len()));
    Some(block_for(&shown, &text))
}

/// What the model is told when a subtree's rules did not fit.
///
/// Said rather than swallowed: a subtree whose rules were declined looks exactly like
/// one that has none, and the difference is worth a sentence.
fn over_budget(shown: &str) -> String {
    format!(
        "{shown} was not read: this run's {MAX_BYTES}-byte instruction budget is \
         spent. Start afi in that directory, or name the file with \
         --instructions <path,...>, to send it instead."
    )
}

/// The directory a tool call's path belongs to: itself when it is one, its parent
/// otherwise - a `write_file` names a file that does not exist yet.
fn dir_of(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return Some(path.to_path_buf());
    }
    path.parent().map(Path::to_path_buf)
}

/// A path resolved through links and `..` where the filesystem can say so, so two
/// spellings of one file are one entry in `sent`.
fn canonical<P: AsRef<Path>>(path: P) -> PathBuf {
    let path = path.as_ref();
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests;
