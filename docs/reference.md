# Reference

Flags, environment variables, subcommands, and slash commands for `afi`. See the [main README](../README.md) for setup and concepts.

## Flags

| flag                                        | what it does                                                |
| ------------------------------------------- | ----------------------------------------------------------- |
| `--yolo`                                    | start in never-prompt mode (auto-approve everything)        |
| `--approval <all\|low\|medium\|high\|yolo>` | start with a non-default approval mode                      |
| `--source <name>`                           | start on a specific source                                  |
| `--resume [target]`                         | resume a saved session - bare = most recent                 |
| `--session <id>`                            | start a fresh run attached to a specific session id         |
| `--prompt-file <path>` / `-f`               | non-interactive single-shot mode (reads from file or stdin) |
| `--summary json`                            | print a machine-readable run summary on stdout ([details](#run-summary)) |
| `--summary-file <path>`                     | also write that summary to a path ([details](#writing-the-summary-to-a-file)) |
| `--effort <low\|medium\|high\|xhigh\|max>`  | how hard the model is asked to think ([details](#reasoning-effort))  |
| `--read-only`                               | deny every tool that can change anything ([details](#tool-policy)) |
| `--allowed-tools <a,b>`                     | only these tools may be called ([details](#tool-policy))    |
| `--disallowed-tools <a,b>`                  | these tools may not be called ([details](#tool-policy))     |
| `--version` / `-V`                          | print the version and build metadata ([details](#version-and-build-metadata)) |
| `--help` / `-h`                             | print usage and exit                                        |

`--help` and `--version` are answered before anything else, so neither depends on an env file loading or a source resolving, and both work as the last word of a command you were already typing. `--help` wins when both are given.

## Version and build metadata

`afi --version` identifies the exact binary, one `label: value` per line so it can be read with `grep` rather than a JSON parser:

```
afi 0.2.0
  commit:      eab85680abce54c56e1ce07f0c51208288ab7f02
  commit-date: 2026-08-06T17:25:27+02:00
  target:      aarch64-apple-darwin
  profile:     release
  rustc:       1.97.1
  executable:  /home/you/.cargo/bin/afi
  sha256:      1cec5f3a9eb9663b5d9cce1d12d1b8c792cdc6f38d8d3504fd6203137a294be7
```

`sha256` is the digest of the executable itself, computed as it runs, so it matches `sha256sum $(command -v afi)` and can be recorded from a CI log to prove which binary produced a result. It is not the digest of the release tarball, which covers the archive rather than the file inside it.

`commit` carries a `(dirty)` marker when tracked files had been modified relative to that commit, since otherwise the sha would describe code that was never compiled. `commit-date` is the commit's own date rather than a build timestamp: cargo caches build-script output, so a "built at" stamp would record when the script last ran and drift away from the binary beside it.

Every field except the version and the profile is best-effort. A build with no git repository, or with no `git` binary at all - a release tarball, `cargo install` from a registry, a container holding only the sources - reports `unknown` instead of failing to compile. `AFI_BUILD_COMMIT` and `AFI_BUILD_COMMIT_DATE` pin those two fields at build time for exactly that case, and `GITHUB_SHA` is picked up automatically, so CI on the runner itself needs no wiring. A build inside a container has to pass the variable in, since neither the runner's environment nor its git repository is visible there by default.

## Environment variables

| env var                                      | what it does                                                         |
| -------------------------------------------- | -------------------------------------------------------------------- |
| `AFI_APPROVAL`                               | persistent default approval mode: `all`/`low`/`medium`/`high`/`yolo` |
| `AFI_BASE_URL` / `AFI_MODEL` / `AFI_API_KEY` | legacy single-source config                                          |
| `AFI_SOURCES` / `AFI_SOURCE_*`               | named multi-source endpoints                                         |
| `AFI_ACTIVE`                                 | name of the source to start on                                       |
| `AFI_TOGETHER_API_KEY`                       | auto-registers a built-in `together` source                          |
| `AFI_OPENROUTER_API_KEY`                     | auto-registers a built-in `openrouter` source                        |
| `ANTHROPIC_API_KEY`                          | auto-registers a built-in `anthropic` source ([details](#anthropic)) |
| `AFI_BACKEND`                                | set to `vllm` to disable llama.cpp-only recovery knobs               |
| `AFI_HOME` / `AFI_SESSIONS_DIR`              | where session JSON files are stored                                  |
| `AFI_AUTOCOMPRESS_PERCENT`                   | auto-compress threshold (default 85, 0=off)                          |
| `AFI_MAX_TOKENS`                             | token cap for normal streaming requests (default 16000)              |
| `AFI_READ_FILE_LINES`                        | default lines returned by `read_file` (default 400)                  |
| `AFI_TOOL_RESULT_CHARS`                      | per-tool-result char cap (default 20000)                             |
| `AFI_SUMMARY`                                | set to `json` for a run summary on stdout ([details](#run-summary))  |
| `AFI_SUMMARY_FILE`                           | path to also write the run summary to ([details](#writing-the-summary-to-a-file)) |
| `AFI_EFFORT`                                 | reasoning effort for every source ([details](#reasoning-effort))     |
| `AFI_ALLOWED_TOOLS` / `AFI_DISALLOWED_TOOLS` | restrict which tools a run may call ([details](#tool-policy))         |
| `AFI_READ_ONLY`                              | deny every tool that can change anything ([details](#tool-policy))    |
| `AFI_PRICES`                                 | per-model token rates, so the summary reports cost ([details](#cost)) |

## Tool policy

`--read-only`, `--allowed-tools`, and `--disallowed-tools` (or `AFI_READ_ONLY`, `AFI_ALLOWED_TOOLS`, `AFI_DISALLOWED_TOOLS`) bound what a run can reach, independently of approval. A flag wins over its variable, except `--read-only`, which only ever turns the posture on.

```
afi --read-only -f review-prompt.txt
```

That is the whole posture for a job that reads. `--read-only` leaves `read_file` and `list_dir` and denies everything else: the two writers, the shell, and `wait_background`, which deletes the log it hands back. Approval only ever asks about the writers and the shell, so a read-only run has nothing left to prompt for and needs no approval bypass. **It does not need `--yolo`, and pairing the two grants nothing** - the flag would only decide whether afi asks about tools the run can no longer call.

Approval alone cannot express "read but do not write": it decides whether afi *asks*, while the policy decides what exists to ask about. A run that genuinely must write still needs approval settled, and that is the one case for `--yolo`; give it a tool policy too, so "do not ask me" does not also mean "anything at all".

Prefer `--read-only` to spelling out an allow list. It names no tools, so it cannot be mistyped, and it is a denial, so it cannot be widened: `--read-only --allowed-tools run_bash` still leaves `run_bash` blocked. A new mutating tool is covered the day it is added, because the posture and the approval gate read the same list.

An absent or blank list means every tool, so `AFI_ALLOWED_TOOLS=""` from an unset shell variable is not a lockout. A non-empty allow list is exhaustive. Deny always wins, so `--allowed-tools read_file,run_bash --disallowed-tools run_bash` leaves only `read_file`. Names accept commas or whitespace and are case-insensitive. The tools are `read_file`, `write_file`, `edit_file`, `list_dir`, `run_bash`, and `wait_background`.

**A policy that cannot be honoured exits 2 without starting.** A mistyped `--disallowed-tools run_bsah` would otherwise match nothing and leave `run_bash` available while the command line claimed otherwise. A flag with no value is refused the same way, since `--disallowed-tools $DENY` with `DENY` unset would grant everything.

Enforced in two places. Blocked tools are left out of the request, so the model has no schema to call. Dispatch then refuses them regardless, and that is the gate that actually holds: the text protocol parses calls out of prose, so a model can name a tool it was never offered, and the built-in system prompt describes `run_bash` and `wait_background` in prose besides. A blocked call cannot reach the filesystem or the shell even when it arrives. The refusal goes back as a tool result naming the permitted tools, so the turn continues instead of stalling.

`final_answer` is never blockable. It carries the forced-final answer rather than doing anything, so blocking it would strand a run rather than restrict it.

A restricted run shows `tools:` in the status line and lists the permitted set in the [run summary](#run-summary). An unrestricted one shows neither, so the segment appearing is itself the signal.

**This is not a sandbox.** It bounds which afi tools run, not what a permitted command does once started. A permitted `run_bash` can do anything the user can, including editing files, and nothing stops it unsetting these variables for a nested `afi`. Use it to keep a run inside the shape you intended, not to contain something adversarial.

## Reasoning effort

`--effort <level>` (or `AFI_EFFORT`) says how hard the model should think. The levels are `low`, `medium`, `high`, `xhigh`, and `max`, and the flag wins over the variable.

```
afi --effort xhigh -f review-prompt.txt
```

The same level reaches every source in whatever its endpoint calls it:

| endpoint                     | sent as                              | highest level |
| ---------------------------- | ------------------------------------ | ------------- |
| Anthropic Messages API       | `output_config: {"effort": "…"}`     | `max`         |
| OpenRouter                   | `reasoning: {"effort": "…"}`         | `high`        |
| OpenAI                       | `reasoning_effort: "…"`              | `high`        |
| everything else              | nothing                              | -             |

A level above an endpoint's ceiling is capped rather than sent, and a source with no effort control afi knows of - llama.cpp, vLLM, SGLang, Z.ai - gets nothing at all. Both print a line on stderr naming the source, and neither stops the run: a level is a preference the endpoint may simply not have, and dying over it would make the flag unusable in any script that switches source. Only the source the run starts on is reported; `/source` switches to an endpoint with a different ladder without saying so.

Talking to OpenAI's own API also switches the output limit from `max_tokens` to `max_completion_tokens`, since its reasoning models - the only ones `reasoning_effort` applies to - reject the older key outright. Every other endpoint keeps `max_tokens`, the only spelling a self-hosted server implements.

The ceilings above belong to the wire formats, which are stable. **Individual models are stricter, and afi keeps no table of that** - `claude-haiku-4-5` takes no effort at all, and older Opus stops at `high`. A model that rejects a level says so on the first request, which is a clearer answer than a compiled-in list nobody notices going stale.

**An unusable level exits 2 without starting**, whether it came from the flag or the variable. This is the reason to prefer it over hand-writing the same JSON into `EXTRA_BODY`, where a typo is warned about and ignored: a run at an effort nobody asked for finishes normally and looks exactly like one at the right effort, so there is nothing downstream to notice.

`EXTRA_BODY` stays the escape hatch and wins wherever the two would meet. afi never overwrites a level written there by hand, and it never adds one to an object written there either: `{"reasoning":{"max_tokens":2000}}` is left exactly as it is rather than becoming `{"max_tokens":2000,"effort":"high"}`, because OpenRouter documents those two keys as mutually exclusive and afi cannot know which keys any given endpoint pairs that way. Either case prints a line on stderr, and the [run summary](#run-summary) reports whichever level the requests actually carried.

On the Anthropic path one default gives way. `thinking` is sent as `disabled` unless [`AFI_ANTHROPIC_EXTRA_BODY`](#anthropic) says otherwise, and `claude-opus-5` rejects an explicit `disabled` above effort `high`; at `xhigh` and `max` the key is therefore omitted, leaving the model at its own default. Anything explicit in `EXTRA_BODY` is still sent as written, `disabled` included.

**Thinking is charged against `max_tokens`, so the floor moves with it.** Anthropic caps thinking and visible text with one number, and afi's forced-final turn asks for only 2048. Whenever a request may think - because `EXTRA_BODY` turned it on, or because the effort is above `high` - that request's `max_tokens` is floored at 16000 rather than 4096, so the budget cannot go entirely on reasoning and leave nothing to say. Higher effort wants more than the floor, and `AFI_MAX_TOKENS` is how to give it (Anthropic's own guidance is 64000 at `xhigh` and `max`). A turn that ends with no answer at all now prints `FORCED FINAL RETURNED NO ANSWER`, exits 1, and reports `"ok": false` rather than a successful empty answer.

## Run summary

`--summary json` (or `AFI_SUMMARY=json`) prints one JSON object on stdout after a non-interactive run, for CI that needs the result rather than the rendered transcript:

```json
{
  "ok": true,
  "error": null,
  "source": "anthropic",
  "model": "claude-sonnet-5",
  "answer": "…the model's final text…",
  "usage": {
    "input_tokens": 1847,
    "output_tokens": 484,
    "cache_read_tokens": 6837,
    "cache_write_tokens": 2279,
    "reasoning_tokens": 0,
    "total_tokens": 11447,
    "requests": 3,
    "cost_usd": 0.023398
  },
  "elapsed_secs": 12.17,
  "tools": ["read_file", "write_file", "edit_file", "list_dir", "run_bash", "wait_background"],
  "effort": "xhigh"
}
```

`answer` is the last assistant message with text, so a review flow can post it directly. Turns that only called tools are skipped.

`tools` is what the run was permitted to call, so an audit of a CI log can confirm the [tool policy](#tool-policy) from the output instead of trusting that the workflow passed the flag it claims to.

`effort` is there for the same reason: it is the level the requests actually carried, read back off the source rather than off the flag, so a capped level reads as the capped one and a level set by hand in `EXTRA_BODY` still shows up. `null` means the run took the endpoint's own default - either nobody asked for a level, or that endpoint has no [effort control](#reasoning-effort) afi knows of.

The five token counts are disjoint and sum to `total_tokens`. They are per-run totals across every billed request, which is what a provider charges for: each turn resends the whole history. `requests` counts those requests - a model turn is one, and so is a compression request, which is why it is not called `turns`. `usage` is `null` rather than a row of zeros when nothing reported any, so a caller can tell a silent provider from a free run.

`cache_write_tokens` is separate from `cache_read_tokens` and from `input_tokens` because the three are priced differently - Anthropic bills a write above base input and a read far below it, so a cost calculation needs its own rate for each. Only the Anthropic path reports writes; an OpenAI-compatible source reports `0`, as does llama.cpp, whose `timings.cache_n` counts a reused prefix and is therefore a read.

Reporting writes separately re-attributes tokens rather than adding them. The 2279 above used to sit inside `input_tokens`, which is why it comes out of that count and leaves `total_tokens` where it was.

Anthropic prices a 5-minute cache write differently from a 1-hour one and reports them separately. `afi` only ever requests the default TTL, so the single figure here covers every write it can make.

`cost_usd` appears only when you supply rates - see [Cost](#cost) below.

A failed run sets `ok` to false, fills in `error`, and exits 1.

Both non-interactive entry points report it: `--prompt-file`, and piped stdin with no prompt file. A piped session summarizes the whole session, so `answer` is its last assistant text and `usage` covers every request it made, `/compress` included; any turn failing outright makes the run fail, `/recover` included. An interactive TTY session prints nothing extra and always exits 0 — stdout there is the rendered interface, and a human is already reading it.

**Human output moves to stderr** while `--summary json` is set, so stdout holds nothing but the JSON and pipes straight into a parser. Errors go to stderr either way.

## Writing the summary to a file

`--summary-file <path>` (or `AFI_SUMMARY_FILE=<path>`) writes the same object to a path:

```bash
afi -f review-prompt.txt --summary-file "$RUNNER_TEMP/afi-run.json"
jq -r .answer "$RUNNER_TEMP/afi-run.json"
```

**It does not imply `--summary json`.** The two are asked for separately, and stdout keeps the rendered run unless you also ask for the JSON there. That is the reason to want a file: capturing stdout to get the summary costs the readable output, so a workflow that wants both ends up redirecting stdout to a file and reading the answer back out to print it, and the human view becomes a copy of the machine one. Pass both flags to get both channels; they render one object built once, so the file and the pipe cannot disagree about what the run did.

It also takes stdout out of the failure surface. Anything touching the pipe between afi and the parser corrupts the only machine copy - a wrapper, a `tee`, a shell that prints one line of its own. A path is addressed rather than piped, so a caller can upload it as a build artifact without capturing anything.

The file is written to a sibling temp file, flushed, and renamed into place, so a reader that opens the path sees either nothing or one complete object, never the prefix of one still being written. It holds the object and a trailing newline. A rerun replaces it. The temp name is unpredictable and is created with `O_EXCL`, so another local user who can write the directory - a shared runner workspace, a mounted volume - cannot plant a symlink there and have afi truncate and overwrite whatever it points at.

**A path that cannot be written refuses to start**, exiting 2 with the reason, before the run is paid for. A missing directory, an unwritable one, a directory in place of the file, and a path ending in `/` are all found by touching the path at startup; the existing file, if there is one, is left alone until there is a whole object to replace it with. A write that fails anyway - the directory went away mid-run - reports on stderr and exits 1. Falling back to stdout would be no fallback at all, because a caller that named a path is not watching stdout for the answer.

The flag is stricter than the variable about being given nothing. `--summary-file` with no value, with a blank value, or with something that looks like another flag exits 2 the way a broken [tool policy](#tool-policy) does. Both `afi --summary-file $OUT` and `afi --summary-file "$OUT"` with `OUT` unset are refused - the quoted form arrives as an empty argument rather than as no argument, and it is the form a CI script is written in. Either would otherwise exit 0 having written nothing to the path the next step is about to read, or leave a file from an earlier run standing as this run's result. A blank `AFI_SUMMARY_FILE` names no file and is not an error, since that is what an exported-but-unset shell variable looks like.

The same entry points report it as `--summary json` does, and an interactive TTY session writes no file.

## Cost

No provider afi speaks to returns a cost. Anthropic's Messages API reports tokens, and so does every OpenAI-compatible endpoint, so the rates have to come from somewhere - and a table compiled into afi is a table nobody notices going stale. You supply it in `AFI_PRICES`, a JSON object mapping model id to USD per million tokens:

```bash
export AFI_PRICES='{
  "claude-sonnet-5": {"input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75}
}'
```

The summary then carries `usage.cost_usd`, rounded to the micro-dollar. Without it there is no `cost_usd` key at all - not a null, not a zero, both of which read as "this run was free" to anything summing the field.

The four classes match the token counts they price. `reasoning` is a fifth, optional key; leave it out and reasoning tokens are billed at the `output` rate, which is what every provider here does.

A class you leave unpriced is fine as long as the run spent nothing there - an OpenAI-compatible source reports `0` cache writes on every request, so demanding a write rate would suppress every figure. Spend tokens on an unpriced class, or on a model missing from the table, and `cost_usd` disappears rather than reporting the part it could price.

Model ids match case-insensitively after trimming, and must otherwise be exactly the id afi sends to the provider - what `model` shows in the summary, or what `/source` reports. A mismatch drops the field, which is the point: an absent number is checkable, a wrong one is not.

Rates are read as exact decimals, down to the sixth place - a millionth of a dollar per million tokens, which is a hundredth of a micro-dollar on a ten-million-token run. Exponent notation is read as the number it denotes, so `3e-1` and `0.3` are the same rate.

Four things warn at startup and disable cost reporting for the whole run: a negative rate, a rate finer than the sixth decimal place or too large to hold, a misspelled class key, and a model named twice. The last one counts case and surrounding space as the same id, so `{"M": ..., "m": ...}` is a duplicate - one of the two would otherwise win at random and the bill would change between runs. One unreadable entry is not priced around.

A session that switches models is billed against each model's own rates, so `cost_usd` stays right even though `model` can only name the last one.

## Anthropic

Every other source speaks OpenAI-compatible `/chat/completions`. Anthropic speaks its own Messages API (`POST /v1/messages`): real `tool_use` blocks, adaptive thinking, and a cached system prompt. Sessions, `/compress`, and transcripts are unchanged.

Set one credential and an `anthropic` source registers itself, defaulting to `https://api.anthropic.com` and `claude-sonnet-5`.

| env var                                                                                       | auth mode                          |
| --------------------------------------------------------------------------------------------- | ---------------------------------- |
| `AFI_ANTHROPIC_API_KEY` or `ANTHROPIC_API_KEY`                                                | `x-api-key`                        |
| `AFI_ANTHROPIC_OAUTH_TOKEN` or `ANTHROPIC_AUTH_TOKEN`                                         | pre-minted `Authorization: Bearer` |
| `ANTHROPIC_FEDERATION_RULE_ID` + `ANTHROPIC_ORGANIZATION_ID` + `ANTHROPIC_SERVICE_ACCOUNT_ID` | workload identity federation       |

An API key wins over a bearer token, and a bearer token over federation, matching the official SDKs. The un-prefixed `ANTHROPIC_*` names are read too, so a shell already configured for the Anthropic SDKs or the `ant` CLI works as it stands. Override the endpoint and model with `AFI_ANTHROPIC_BASE_URL` and `AFI_ANTHROPIC_MODEL`, or per switch with `/source anthropic claude-opus-5`.

Those overrides deliberately sit outside the `AFI_SOURCE_*` namespace, which is reserved for sources you define yourself. A bare `AFI_SOURCE_ANTHROPIC_BASE_URL` is enough for afi to auto-discover a source named `anthropic`, and it would come up on the OpenAI-compatible protocol with no credential. Defining a full `AFI_SOURCE_ANTHROPIC_*` block still works, but then set `AFI_SOURCE_ANTHROPIC_PROTOCOL` too.

**Federation.** afi exchanges an OIDC identity token for an access token at `/v1/oauth/token`, then re-mints it near expiry. Add `ANTHROPIC_WORKSPACE_ID` when the rule spans workspaces. The identity token comes from `ANTHROPIC_IDENTITY_TOKEN`, else `ANTHROPIC_IDENTITY_TOKEN_FILE`, else GitHub Actions' OIDC endpoint, so a workflow granting `id-token: write` mints nothing itself:

```yaml
permissions:
  contents: read
  id-token: write
steps:
  - uses: actions/checkout@v7
  - run: afi --read-only -f prompt.txt
    env:
      AFI_ACTIVE: anthropic
      ANTHROPIC_FEDERATION_RULE_ID: fdrl_...
      ANTHROPIC_ORGANIZATION_ID: ...
      ANTHROPIC_SERVICE_ACCOUNT_ID: svac_...
```

A read-only job needs nothing else: there is no terminal to answer a prompt, and `--read-only` leaves nothing that would raise one. A job that has to write does need `--yolo` or `AFI_APPROVAL=yolo`, or every write and bash call is denied rather than hanging - pair it with a [tool policy](#tool-policy) so "do not ask me" does not also mean unrestricted.

**Sampling parameters stay off the wire.** Anthropic rejects `temperature`, `top_p`, and `top_k`, and `min_p` and the DRY knobs belong to llama.cpp, so recovery falls back to its prompt-level nudges. `AFI_ANTHROPIC_EXTRA_BODY` accepts `output_config`, `metadata`, `stop_sequences`, and `service_tier`, and drops the rest.

**Thinking is off by default, and `AFI_ANTHROPIC_EXTRA_BODY` turns it on.** The `thinking` key has three states:

| `thinking` in `EXTRA_BODY` | sent as | for |
| ---------------------------- | --------------------------- | -------------------------------------------- |
| absent                       | `{"type": "disabled"}`      | the default; the only shape `claude-haiku-4-5` accepts |
| absent, at effort `xhigh` or `max` | omitted entirely      | `claude-opus-5`, which rejects an explicit `disabled` that high ([details](#reasoning-effort)) |
| `null`                       | omitted entirely            | `claude-fable-5`, which rejects an explicit `disabled` and always thinks |
| an object                    | verbatim                    | `{"type": "adaptive", "display": "summarized"}` |

```bash
AFI_ANTHROPIC_EXTRA_BODY='{"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"effort":"high"}}'
```

Disabled stays the default because it is the one value every current model accepts: `claude-haiku-4-5` rejects adaptive outright, and on `claude-opus-5` disabling thinking is only allowed at effort `high` or below.

`display` decides what you see. The API's default, `omitted`, still thinks and still bills for it but returns empty text, so the reasoning pane stays blank and the turn looks like a long pause. `summarized` streams a readable summary.

**Thinking blocks round-trip.** When a thinking block accompanies a tool call, the API requires the assistant turn echoed back verbatim on the request carrying the tool result — block, text, and signature. afi stores the raw blocks under an `afi_thinking` key on the assistant turn that made the calls, and replays them ahead of the `tool_use` they belong to. That is the only turn that needs them; a plain text answer ends the exchange, so nothing is kept for it. Sessions carry the key (schema stays `afi-1`; a session written by an older afi simply has none), and it is stripped from every OpenAI-protocol request, since it is not part of that wire format.

Three cases lose a block rather than risk the turn: a stream cut before the signature arrived, a `/compress` that sliced away the tool result the reasoning was aimed at, and a request that turns thinking back off. Anthropic validates the whole request, so one unusable block would fail the turn instead of being ignored.

**The reasoning-only cut is off while thinking is on.** `AFI_REASONING_ONLY_CHARS` exists for local models that loop in their scratchpad forever; Anthropic's thinking is server-side and already bounded by `max_tokens`, so cutting one of those turns short would fire on a healthy turn that was about to emit its tool call.

**Other endpoints.** `AFI_SOURCE_<NAME>_PROTOCOL` takes `anthropic` or `anthropic-oauth`. It defaults to `openai`, so existing sources keep working.

## Subcommands

| subcommand             | what it does                                                                  |
| ---------------------- | ----------------------------------------------------------------------------- |
| `afi`                  | start the REPL                                                                |
| `afi sessions [query]` | list saved sessions, 10 per page (prints + exits) - optional substring filter |

## Commands

| command                             | what it does                                                    |
| ----------------------------------- | --------------------------------------------------------------- |
| `/source [name] [model]`            | list sources, switch to one, or override its model              |
| `/yolo`                             | toggle auto-approve for writes and bash                         |
| `/approval [level]`                 | show or set risk threshold (`all`/`low`/`medium`/`high`/`yolo`) |
| `/sessions [n]`                     | list recent sessions, or show one in full                       |
| `/save [title]`                     | save the current session (optional custom title)                |
| `/delete [target]`                  | delete a saved session                                          |
| `/compress`                         | summarize older turns into one, keep last 2 verbatim            |
| `/compact`                          | alias for `/compress`                                           |
| `/autocompress [pct\|off\|on]`      | show or set the auto-compress threshold                         |
| `/reset`                            | clear conversation, start a fresh session                       |
| `/clear`                            | alias for `/reset`                                              |
| `/new`                              | alias for `/reset`                                              |
| `/memory save\|remember\|list`      | manage developer memories                                       |
| `/recover [note]`                   | force a low-temp visible checkpoint after a bad stream          |
| `/provider [source] [a,b,...\|off]` | show or set OpenRouter provider-routing order                   |
| `/help`                             | show available commands                                         |
| `/quit`                             | exit                                                            |
