//! CLI argument parsing: the subset of argv that affects initial runtime state.
//!
//! Split out of `runtime` so the flag table can grow without crowding the
//! session state it feeds. The `sessions` subcommand and the in-REPL slash
//! commands are parsed elsewhere.

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
    pub allowed_tools: Option<String>,
    pub disallowed_tools: Option<String>,
    pub effort: Option<String>,
    /// Flags that were given wrongly. Only the flags whose silent fallback
    /// loses something the caller asked for record here - see `set_required`.
    pub flag_errors: Vec<String>,
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
        "--summary-file" => return set_summary_file(out, value),
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
/// and is left unconsumed so the following flag still applies.
fn set_required(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> Option<String> {
    let Some(v) = value.filter(|v| !v.starts_with('-')) else {
        out.flag_errors.push(format!("{flag} needs a value"));
        return None;
    };
    Some(v.to_string())
}

/// Set `--summary-file`. Returns whether a value was consumed.
///
/// Blank is refused as well as absent, which `set_required` alone does not do:
/// `"".starts_with('-')` is false, so an empty argument passes that filter.
/// `afi --summary-file "$OUT"` with `OUT` unset passes exactly that - the quoted
/// form is how a CI script is written - and accepting it would exit 0 having
/// written nothing to the path the next step reads, or leave a file from an
/// earlier run standing as this run's result. The unquoted `$OUT` drops the
/// argument entirely and was already refused; both spellings of the same mistake
/// now fail the same way.
///
/// Blank stays permitted for `AFI_SUMMARY_FILE`, where an exported but unset
/// variable is how a workflow turns the feature off - see `summary_path`. The
/// tool-policy flags keep the looser rule too, because a blank list there is
/// documented as "every tool" rather than as a mistake.
fn set_summary_file(out: &mut ParsedArgs, value: Option<&str>) -> bool {
    let Some(path) = set_required(out, "--summary-file", value) else {
        return false;
    };
    if path.trim().is_empty() {
        out.flag_errors
            .push("--summary-file needs a value".to_string());
        // Consumed all the same: the argument was there, it just said nothing.
        return true;
    }
    out.summary_file = Some(path);
    true
}

/// Set `--effort`. Returns whether a value was consumed.
///
/// The level itself is validated later, against the sources it has to reach -
/// see `config::effort`. All this decides is that one was given.
fn set_effort(out: &mut ParsedArgs, value: Option<&str>) -> bool {
    let Some(level) = set_required(out, "--effort", value) else {
        return false;
    };
    out.effort = Some(level);
    true
}

/// Set one of the two tool-policy flags. Returns whether a value was consumed.
fn set_tool_flag(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> bool {
    let Some(v) = set_required(out, flag, value) else {
        return false;
    };
    if flag == "--allowed-tools" {
        out.allowed_tools = Some(v);
    } else {
        out.disallowed_tools = Some(v);
    }
    true
}

/// Set `slot` to `value` when present; returns whether a value was consumed.
fn set_opt(slot: &mut Option<String>, value: Option<&str>) -> bool {
    if let Some(v) = value {
        *slot = Some(v.to_string());
        return true;
    }
    false
}
