//! CLI argument parsing: the subset of argv that affects initial runtime state.
//!
//! Split out of `runtime` so the flag table can grow without crowding the
//! session state it feeds. The `sessions` subcommand and the in-REPL slash
//! commands are parsed elsewhere.

use crate::summary::{ErrorKind, RunError};

/// Parsed CLI args - the subset that affects initial state. The `sessions`
/// subcommand and in-REPL slash commands are handled separately.
#[derive(Debug, Default, Clone)]
pub struct ParsedArgs {
    pub source: Option<String>,
    pub yolo: bool,
    pub read_only: bool,
    pub approval: Option<String>,
    pub resume: Option<Option<String>>,
    pub session: Option<String>,
    pub prompt_file: Option<String>,
    pub sessions_query: Option<Vec<String>>,
    pub summary: Option<String>,
    pub summary_file: Option<String>,
    pub system_prompt_file: Option<String>,
    pub system_prompt_mode: Option<String>,
    pub allowed_tools: Option<String>,
    pub disallowed_tools: Option<String>,
    pub effort: Option<String>,
    pub config: Option<String>,
    /// Flags that were given wrongly. Only the flags whose silent fallback
    /// loses something the caller asked for record here - see `set_required`.
    ///
    /// Each carries the kind the summary reports, decided by the flag that
    /// raised it: a tool policy that would end up wider than the command line
    /// asked for is `Policy`, and a path this run cannot use is `Input`.
    pub flag_errors: Vec<RunError>,
}

/// Parse argv into the subset that affects runtime construction.
///
/// Hand-rolled for now so tests can pass `["afi", "--source", "zai"]`
/// directly without a clap dependency at test time. Phase 8 may swap this for
/// a clap-based parser; the surface should stay byte-identical.
#[must_use]
pub fn parse_args(args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs::default();
    let mut saw_sessions = false;
    let mut query: Vec<String> = Vec::new();
    let mut i = 1; // skip argv[0]
    while i < args.len() {
        let a = args[i].as_str();
        if i == 1 && a == "sessions" {
            saw_sessions = true;
        } else if saw_sessions {
            query.push(a.to_string());
        } else if apply_flag(&mut out, a, args.get(i + 1).map(String::as_str)) {
            i += 1;
        }
        i += 1;
    }
    if saw_sessions {
        out.sessions_query = Some(query);
    }
    out
}

/// Apply one flag to `out`. Returns `true` when it consumed the following
/// argument as its value.
fn apply_flag(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> bool {
    match flag {
        "--yolo" => out.yolo = true,
        "--read-only" => out.read_only = true,
        "--approval" => return set_opt(&mut out.approval, value),
        "--source" => return set_opt(&mut out.source, value),
        "--session" => return set_opt(&mut out.session, value),
        "--prompt-file" | "-f" => return set_opt(&mut out.prompt_file, value),
        "--summary" => return set_opt(&mut out.summary, value),
        "--summary-file" => {
            return set_required_value(
                &mut out.flag_errors,
                &mut out.summary_file,
                flag,
                value,
                ErrorKind::Input,
            );
        }
        "--config" => {
            return set_required_value(
                &mut out.flag_errors,
                &mut out.config,
                flag,
                value,
                ErrorKind::Input,
            );
        }
        "--system-prompt-file" => {
            return set_required_value(
                &mut out.flag_errors,
                &mut out.system_prompt_file,
                flag,
                value,
                ErrorKind::Input,
            );
        }
        "--system-prompt-mode" => {
            return set_required_value(
                &mut out.flag_errors,
                &mut out.system_prompt_mode,
                flag,
                value,
                ErrorKind::Input,
            );
        }
        "--allowed-tools" | "--disallowed-tools" => return set_tool_flag(out, flag, value),
        "--effort" => return set_effort(out, value),
        "--resume" | "-r" => {
            // bare --resume, or --resume <target> where target doesn't start
            // with '-' (so `--resume --yolo` doesn't swallow --yolo).
            if let Some(v) = value.filter(|v| !v.starts_with('-')) {
                out.resume = Some(Some(v.to_string()));
                return true;
            }
            out.resume = Some(None);
        }
        _ => {}
    }
    false
}

/// The value of a flag that must have one, recording a refusal when it has none.
///
/// Unlike `set_opt`, a missing value cannot be shrugged off for these flags.
/// `afi --disallowed-tools $DENY` with `DENY` unset would grant every tool while
/// the command line says otherwise, `afi --summary-file $OUT` with `OUT` unset
/// would exit 0 having written nothing to the path a workflow is about to read,
/// and a dropped `--effort $LEVEL` would run at an effort nobody asked for. All
/// three are silent failures in the direction nobody wants, so the run refuses
/// to start instead. A value that looks like another flag - what
/// `--summary-file --yolo` produces - is the same mistake and is refused too,
/// and is left unconsumed so the following flag still applies. A bare `-` is
/// refused here as well, unlike on the `set_opt` path: none of these flags
/// takes one, and a summary file literally named `-` is a typo every time.
///
/// `kind` is what the summary reports for this flag, since the flags that reach
/// here fail for different reasons and a caller retries them differently.
fn set_required(
    out: &mut ParsedArgs,
    flag: &str,
    value: Option<&str>,
    kind: ErrorKind,
) -> Option<String> {
    let Some(v) = value.filter(|v| !v.starts_with('-')) else {
        out.flag_errors
            .push(RunError::new(format!("{flag} needs a value"), kind));
        return None;
    };
    Some(v.to_string())
}

/// Fill `slot` from a flag whose value must be there and must say something.
/// Returns whether a value was consumed; `slot` is untouched on a refusal.
///
/// Blank is refused as well as absent, which `set_required` alone does not do:
/// `"".starts_with('-')` is false, so an empty argument passes that filter.
/// `afi --summary-file "$OUT"` with `OUT` unset passes exactly that - the quoted
/// form is how a CI script is written - and accepting it would exit 0 having
/// written nothing to the path the next step reads, or leave a file from an
/// earlier run standing as this run's result. `--system-prompt-file "$PROMPT"`
/// is the same mistake and costs more: the run would send afi's own prompt while
/// the command line says it is sending its own. The unquoted `$OUT` drops the
/// argument entirely and was already refused; both spellings now fail the same
/// way.
///
/// `--config` refuses both for the same reason: a run that silently forgets the
/// file it was pointed at is configured by something other than what the command
/// line says.
///
/// Blank stays permitted for the matching variables, where an exported but unset
/// variable is how a workflow turns the feature off - see `summary_path`,
/// `ConfigFiles::discover`, and `super::system_prompt::resolve`. The tool-policy flags keep the looser rule
/// too, because a blank list there is documented as "every tool" rather than as
/// a mistake.
///
/// `kind` is what the summary reports, as it is for `set_required`.
fn set_required_value(
    flag_errors: &mut Vec<RunError>,
    slot: &mut Option<String>,
    flag: &str,
    value: Option<&str>,
    kind: ErrorKind,
) -> bool {
    let Some(given) = value.filter(|v| !v.starts_with('-')) else {
        flag_errors.push(RunError::new(format!("{flag} needs a value"), kind));
        return false;
    };
    if given.trim().is_empty() {
        flag_errors.push(RunError::new(format!("{flag} needs a value"), kind));
        // Consumed all the same: the argument was there, it just said nothing.
        return true;
    }
    *slot = Some(given.to_string());
    true
}

/// Set `--effort`. Returns whether a value was consumed.
///
/// The level itself is validated later, against the sources it has to reach -
/// see `config::effort`. All this decides is that one was given.
fn set_effort(out: &mut ParsedArgs, value: Option<&str>) -> bool {
    // `Input`: an effort with no level is the invocation being wrong, not a tool
    // policy afi cannot honour, and a caller retries the two differently.
    let Some(level) = set_required(out, "--effort", value, ErrorKind::Input) else {
        return false;
    };
    out.effort = Some(level);
    true
}

/// Set one of the two tool-policy flags. Returns whether a value was consumed.
fn set_tool_flag(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> bool {
    let Some(v) = set_required(out, flag, value, ErrorKind::Policy) else {
        return false;
    };
    if flag == "--allowed-tools" {
        out.allowed_tools = Some(v);
    } else {
        out.disallowed_tools = Some(v);
    }
    true
}

/// Set `slot` to `value` when there is one; returns whether a value was
/// consumed.
///
/// A flag-shaped token is left alone rather than swallowed. Taking it loses two
/// settings at once: `afi --summary --effort xhigh` set `summary` to
/// `"--effort"` and then dropped `xhigh` as a stray positional, so the run
/// produced no summary *and* ran at an effort nobody asked for - the silent
/// failure `--effort` exists to prevent, reached through a flag that has
/// nothing to do with it. Not consuming costs a missing value here, which every
/// flag on this path already tolerates.
fn set_opt(slot: &mut Option<String>, value: Option<&str>) -> bool {
    let Some(v) = value.filter(|v| !is_another_flag(v)) else {
        return false;
    };
    *slot = Some(v.to_string());
    true
}

/// True for a token that is the next flag rather than this one's value.
///
/// A bare `-` is not one: it is `--prompt-file`'s documented "read the prompt
/// from stdin". `set_required` is stricter and refuses it, because none of the
/// flags on that path takes a dash for a value.
fn is_another_flag(value: &str) -> bool {
    value.starts_with('-') && value != "-"
}
