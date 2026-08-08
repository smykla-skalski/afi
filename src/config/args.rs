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
    pub instructions: Option<String>,
    pub allowed_tools: Option<String>,
    pub disallowed_tools: Option<String>,
    pub effort: Option<String>,
    pub config: Option<String>,
    /// The context window this run measures the auto-compress threshold against,
    /// for every source it touches. Kept as written so `Runtime` reports an
    /// unusable one by name rather than silently running without a window.
    pub context_window: Option<String>,
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
        } else if let Some((flag, inline)) = split_inline(a) {
            // The value is in the token, so the next word is the next argument.
            apply_flag(&mut out, flag, Some(inline), true);
        } else if apply_flag(&mut out, a, args.get(i + 1).map(String::as_str), false) {
            i += 1;
        }
        i += 1;
    }
    if saw_sessions {
        out.sessions_query = Some(query);
    }
    out
}

/// `--flag=value` split into its halves, when it was written that way.
///
/// Both spellings reach the same place afterwards, so a flag that refuses a
/// missing value refuses `--flag=` too. Long flags only: `-f=x` would make the
/// value `=x`, and nobody writes a short flag that way.
fn split_inline(arg: &str) -> Option<(&str, &str)> {
    let (name, value) = arg.strip_prefix("--")?.split_once('=')?;
    if name.is_empty() {
        return None;
    }
    // Rebuild the flag with its dashes so every message names it as it was typed.
    let end = 2 + name.len();
    Some((&arg[..end], value))
}

/// Flags that are a statement by themselves, so a value written into one is a
/// mistake rather than a setting.
const VALUELESS: [&str; 6] = ["--yolo", "--read-only", "--help", "-h", "--version", "-V"];

/// Apply one flag to `out`. Returns `true` when it consumed the following
/// argument as its value.
///
/// `inline` says the value came from `--flag=value` rather than from the next
/// word, which is the only thing the two spellings differ by: a flag that takes
/// no value has to refuse one written into it. `--read-only=false` reads as "off"
/// to whoever typed it, and taking the token as a bare `--read-only` would turn
/// the posture on instead.
fn apply_flag(out: &mut ParsedArgs, flag: &str, value: Option<&str>, inline: bool) -> bool {
    if inline && VALUELESS.contains(&flag) {
        out.flag_errors.push(RunError::new(
            format!("{flag} takes no value"),
            ErrorKind::Input,
        ));
        return false;
    }
    if let Some(consumed) = apply_value_flag(out, flag, value) {
        return consumed;
    }
    match flag {
        "--yolo" => out.yolo = true,
        "--read-only" => out.read_only = true,
        // Answered by `cli_meta` long before this, and here only so that writing
        // one wrongly is not reported as a flag afi has never heard of.
        "--help" | "-h" | "--version" | "-V" => {}
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
        // A flag afi does not have is a typo, and every one of them used to be
        // ignored: `--red-only` left a run with writes enabled while the command
        // line said otherwise. A bare word is the same mistake - afi reads its
        // prompt from `-f`, never from a positional.
        other => {
            out.flag_errors.push(RunError::new(
                format!("unknown argument {other:?}"),
                ErrorKind::Input,
            ));
        }
    }
    false
}

/// Fill the field a value-taking flag names, or `None` when `flag` names none.
///
/// These arms differ only in which field they write, so they sit together rather
/// than eight at a time in [`apply_flag`]. Each borrows `flag_errors` and its own
/// field, which are disjoint - routing the field through a helper would borrow the
/// whole of `out` twice.
fn apply_value_flag(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> Option<bool> {
    Some(match flag {
        "--approval" => set_required_value(&mut out.flag_errors, &mut out.approval, flag, value),
        "--source" => set_required_value(&mut out.flag_errors, &mut out.source, flag, value),
        "--session" => set_required_value(&mut out.flag_errors, &mut out.session, flag, value),
        "--summary" => set_required_value(&mut out.flag_errors, &mut out.summary, flag, value),
        "--summary-file" => {
            set_required_value(&mut out.flag_errors, &mut out.summary_file, flag, value)
        }
        "--config" => set_required_value(&mut out.flag_errors, &mut out.config, flag, value),
        "--context-window" => {
            set_required_value(&mut out.flag_errors, &mut out.context_window, flag, value)
        }
        "--system-prompt-file" => set_required_value(
            &mut out.flag_errors,
            &mut out.system_prompt_file,
            flag,
            value,
        ),
        "--system-prompt-mode" => set_required_value(
            &mut out.flag_errors,
            &mut out.system_prompt_mode,
            flag,
            value,
        ),
        "--instructions" => {
            set_required_value(&mut out.flag_errors, &mut out.instructions, flag, value)
        }
        "--prompt-file" | "-f" => set_prompt_file(out, flag, value),
        _ => return None,
    })
}

/// Set `--prompt-file`, which alone among the required-value flags takes a bare
/// `-` - its documented "read the prompt from stdin".
fn set_prompt_file(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> bool {
    if value == Some("-") {
        out.prompt_file = Some("-".to_string());
        return true;
    }
    set_required_value(&mut out.flag_errors, &mut out.prompt_file, flag, value)
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
/// the command line says it is sending its own. `--instructions "$RULES"` is that
/// one again - a review job following none of the rules it was pointed at reads as
/// a job with nothing to say about them. The unquoted `$OUT` drops the argument
/// entirely and was already refused; both spellings now fail the same way.
///
/// `--config` refuses both for the same reason: a run that silently forgets the
/// file it was pointed at is configured by something other than what the command
/// line says.
///
/// Blank stays permitted for the matching variables, where an exported but unset
/// variable is how a workflow turns the feature off - see `summary_path`,
/// `super::file::config_files`, and `super::system_prompt::resolve`. The
/// tool-policy flags keep the looser rule too, because a blank list there is
/// documented as "every tool" rather than as a mistake.
///
/// Every flag here reports `Input`: each one is the invocation naming something
/// this run cannot use, and retrying it lands in the same place.
fn set_required_value(
    flag_errors: &mut Vec<RunError>,
    slot: &mut Option<String>,
    flag: &str,
    value: Option<&str>,
) -> bool {
    let Some(given) = value.filter(|v| !v.starts_with('-')) else {
        flag_errors.push(RunError::new(
            format!("{flag} needs a value"),
            ErrorKind::Input,
        ));
        return false;
    };
    if given.trim().is_empty() {
        flag_errors.push(RunError::new(
            format!("{flag} needs a value"),
            ErrorKind::Input,
        ));
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
