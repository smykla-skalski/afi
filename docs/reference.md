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
| `--allowed-tools <a,b>`                     | only these tools may be called ([details](#tool-policy))    |
| `--disallowed-tools <a,b>`                  | these tools may not be called ([details](#tool-policy))     |

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
| `AFI_ALLOWED_TOOLS` / `AFI_DISALLOWED_TOOLS` | restrict which tools a run may call ([details](#tool-policy))         |

## Tool policy

`--allowed-tools` and `--disallowed-tools` (or `AFI_ALLOWED_TOOLS` / `AFI_DISALLOWED_TOOLS`) bound what a run can reach, independently of approval. The flag wins over the variable.

```
afi --yolo --allowed-tools read_file,list_dir -f review-prompt.txt
```

Approval alone cannot express "read but do not write". Unattended there is nobody to answer a prompt, so anything above the threshold is denied and the run flails; the usual fix is `--yolo`, which grants `write_file`, `edit_file`, and `run_bash` in full. That is right for a human at a prompt and wrong for a job reading an untrusted pull-request diff. The two lists are that missing middle: `--yolo` still decides whether afi *asks*, while the policy decides what exists to ask about.

An absent or blank list means every tool, so `AFI_ALLOWED_TOOLS=""` from an unset shell variable is not a lockout. A non-empty allow list is exhaustive. Deny always wins, so `--allowed-tools read_file,run_bash --disallowed-tools run_bash` leaves only `read_file`. Names accept commas or whitespace and are case-insensitive. The tools are `read_file`, `write_file`, `edit_file`, `list_dir`, `run_bash`, and `wait_background`.

**A name that matches no tool exits 2 without starting.** A mistyped `--disallowed-tools run_bsah` would otherwise match nothing and leave `run_bash` available while the command line claimed otherwise.

Enforced in two places. Blocked tools are left out of the request, so the model is never told they exist and does not spend a turn trying them. Dispatch then refuses them regardless, because the text protocol parses calls out of prose and a model can name a tool it was never offered - so a blocked call cannot reach the filesystem or the shell even when it arrives. The refusal goes back as a tool result naming the permitted tools, so the turn continues instead of stalling.

`final_answer` is never blockable. It carries the forced-final answer rather than doing anything, so blocking it would strand a run rather than restrict it.

A restricted run shows `tools:` in the status line and lists the permitted set in the [run summary](#run-summary). An unrestricted one shows neither, so the segment appearing is itself the signal.

**This is not a sandbox.** It bounds which afi tools run, not what a permitted command does once started. A permitted `run_bash` can do anything the user can, including editing files, and nothing stops it unsetting these variables for a nested `afi`. Use it to keep a run inside the shape you intended, not to contain something adversarial.

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
    "requests": 3
  },
  "elapsed_secs": 12.17,
  "tools": ["read_file", "write_file", "edit_file", "list_dir", "run_bash", "wait_background"]
}
```

`answer` is the last assistant message with text, so a review flow can post it directly. Turns that only called tools are skipped.

`tools` is what the run was permitted to call, so an audit of a CI log can confirm the [tool policy](#tool-policy) from the output instead of trusting that the workflow passed the flag it claims to.

The five token counts are disjoint and sum to `total_tokens`. They are per-run totals across every billed request, which is what a provider charges for: each turn resends the whole history. `requests` counts those requests - a model turn is one, and so is a compression request, which is why it is not called `turns`. `usage` is `null` rather than a row of zeros when nothing reported any, so a caller can tell a silent provider from a free run.

`cache_write_tokens` is separate from `cache_read_tokens` and from `input_tokens` because the three are priced differently - Anthropic bills a write above base input and a read far below it, so a cost calculation needs its own rate for each. Only the Anthropic path reports writes; an OpenAI-compatible source reports `0`, as does llama.cpp, whose `timings.cache_n` counts a reused prefix and is therefore a read.

Reporting writes separately re-attributes tokens rather than adding them. The 2279 above used to sit inside `input_tokens`, which is why it comes out of that count and leaves `total_tokens` where it was.

Anthropic prices a 5-minute cache write differently from a 1-hour one and reports them separately. `afi` only ever requests the default TTL, so the single figure here covers every write it can make.

**No cost field.** Anthropic returns no cost, so any figure here would come from a price table that goes stale without anyone noticing. Multiply the token counts by rates you control.

A failed run sets `ok` to false, fills in `error`, and exits 1.

Both non-interactive entry points report it: `--prompt-file`, and piped stdin with no prompt file. A piped session summarizes the whole session, so `answer` is its last assistant text and `usage` covers every request it made, `/compress` included; any turn failing outright makes the run fail, `/recover` included. An interactive TTY session prints nothing extra and always exits 0 — stdout there is the rendered interface, and a human is already reading it.

**Human output moves to stderr** while `--summary json` is set, so stdout holds nothing but the JSON and pipes straight into a parser. Errors go to stderr either way.

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
  - run: afi --yolo -f prompt.txt
    env:
      AFI_ACTIVE: anthropic
      ANTHROPIC_FEDERATION_RULE_ID: fdrl_...
      ANTHROPIC_ORGANIZATION_ID: ...
      ANTHROPIC_SERVICE_ACCOUNT_ID: svac_...
```

CI needs `--yolo` or `AFI_APPROVAL=yolo`, or every write and bash call is denied - there is no terminal to answer the prompt, so afi declines rather than hanging. Pair it with a [tool policy](#tool-policy) so `--yolo` does not also mean unrestricted.

**Sampling parameters stay off the wire.** Anthropic rejects `temperature`, `top_p`, and `top_k`, and `min_p` and the DRY knobs belong to llama.cpp, so recovery falls back to its prompt-level nudges. `AFI_ANTHROPIC_EXTRA_BODY` accepts `output_config`, `metadata`, `stop_sequences`, and `service_tier`, and drops the rest.

**Thinking is off, and cannot be turned on yet.** When a thinking block accompanies a tool call, the API requires the assistant turn to be echoed back verbatim on the request carrying the tool result, thinking block and signature included. afi keeps history in OpenAI shape and rebuilds that turn on every request, so it cannot round-trip one. Since afi calls tools on nearly every turn, leaving thinking on would fail most of them. `thinking` is therefore sent as `disabled` and is not in the `EXTRA_BODY` allowlist.

Two consequences. `claude-fable-5` rejects an explicit `disabled`, so it is unusable here for now. And sending `disabled` is what keeps `claude-haiku-4-5` working, since it rejects adaptive thinking outright.

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
