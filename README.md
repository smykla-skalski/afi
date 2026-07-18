# minion

![minion](minion.png)

A no-nonsense coding agent that doesn't use 50K tokens of context to say "hello."

`minion` is a Rust binary that talks to any OpenAI-compatible endpoint - a local
llama.cpp / vLLM / SGLang server, or a remote API like Z.ai or OpenAI itself -
and starts chatting with an agent that can read, write, edit, and run shell
commands in your project.

This is a from-scratch Rust port of the original single-file Python
[`minion.py`](https://github.com/Sentdex/minion). The CLI flags, env vars,
slash commands, and behavior are byte-identical except for two documented
breaking changes (see [CHANGELOG.md](CHANGELOG.md)):

- the traffic log moved from `llamacpp.log` next to the script to
  `~/.minion/logs/traffic.jsonl`
- the `~/.minion/sessions/<id>.json` schema is fresh and version-tagged;
  sessions written by the Python version will not resume

## Quick start

```
cargo install --path .
export MINION_BASE_URL=http://localhost:8080/v1
export MINION_MODEL=your-model-name
export MINION_API_KEY=sk-noop        # any string; local servers ignore it
minion
```

If `MINION_MODEL` is unset, minion asks the server what it's serving.

## Configuration

minion reads configuration from environment variables, and automatically loads
`~/.env` at startup (so you don't have to export things in every terminal).

### Single source (simple)

```
MINION_BASE_URL=http://localhost:8080/v1
MINION_MODEL=your-model-name
MINION_API_KEY=sk-noop
```

### Multiple sources

Define named endpoints and switch between them at runtime:

```
MINION_SOURCES=local,zai

MINION_SOURCE_LOCAL_BASE_URL=http://localhost:8080/v1
MINION_SOURCE_LOCAL_API_KEY=sk-noop

MINION_SOURCE_ZAI_BASE_URL=https://api.z.ai/api/paas/v4
MINION_SOURCE_ZAI_API_KEY=$zai_test         # $name = look up a key from env / ~/.env
MINION_SOURCE_ZAI_MODEL=glm-x-preview
```

See [`sources.example.env`](sources.example.env) for a full annotated example.
Switch at runtime with `/source [name]`.

### Flags

| flag                          | what it does                                              |
| ----------------------------- | -------------------------------------------------------- |
| `--yolo`                      | start in never-prompt mode (auto-approve everything)      |
| `--approval <all\|low\|medium\|high\|yolo>` | start with a non-default approval mode       |
| `--source <name>`             | start on a specific source                                |
| `--resume [target]`           | resume a saved session; bare = most recent                |
| `--session <id>`              | start a fresh run attached to a specific session id       |
| `--prompt-file <path>` / `-f` | non-interactive single-shot mode (reads from file or stdin) |

### Environment variables

| env var | what it does |
| --- | --- |
| `MINION_APPROVAL` | persistent default approval mode: `all`/`low`/`medium`/`high`/`yolo` |
| `MINION_BASE_URL` / `MINION_MODEL` / `MINION_API_KEY` | legacy single-source config |
| `MINION_SOURCES` / `MINION_SOURCE_*` | named multi-source endpoints |
| `MINION_ACTIVE` | name of the source to start on |
| `TOGETHER_API_KEY` | auto-registers a built-in `together` source |
| `OPENROUTER_API_KEY` | auto-registers a built-in `openrouter` source |
| `MINION_BACKEND` | set to `vllm` to disable llama.cpp-only recovery knobs |
| `MINION_HOME` / `MINION_SESSIONS_DIR` | where session JSON files are stored |
| `MINION_AUTOCOMPRESS_PERCENT` | auto-compress threshold (default 85; 0=off) |
| `MINION_MAX_TOKENS` | token cap for normal streaming requests (default 16000) |
| `MINION_READ_FILE_LINES` | default lines returned by `read_file` (default 400) |
| `MINION_TOOL_RESULT_CHARS` | per-tool-result char cap (default 20000) |

## Subcommands

| subcommand          | what it does                                          |
| ------------------- | ---------------------------------------------------- |
| `minion`            | start the REPL                                        |
| `minion sessions [query]` | list saved sessions, 10 per page (prints + exits); optional substring filter |

## Commands

| command             | what it does                                            |
| ------------------- | ------------------------------------------------------ |
| `/source [name] [model]` | list sources, switch to one, or override its model |
| `/yolo`             | toggle auto-approve for writes and bash                 |
| `/approval [level]` | show or set risk threshold (`all`/`low`/`medium`/`high`/`yolo`) |
| `/sessions [n]`     | list recent sessions, or show one in full               |
| `/save [title]`     | save the current session (optional custom title)        |
| `/delete [target]`  | delete a saved session                                  |
| `/compress`         | summarize older turns into one, keep last 2 verbatim     |
| `/compact`          | alias for `/compress`                                    |
| `/autocompress [pct\|off\|on]` | show or set the auto-compress threshold |
| `/reset`            | clear conversation, start a fresh session               |
| `/clear`            | alias for `/reset`                                       |
| `/new`              | alias for `/reset`                                       |
| `/memory save\|remember\|list` | manage developer memories |
| `/recover [note]`   | force a low-temp visible checkpoint after a bad stream   |
| `/provider [source] [a,b,...\|off]` | show or set OpenRouter provider-routing order |
| `/help`             | show available commands                                  |
| `/quit`             | exit                                                     |

## Input

The prompt is a multi-line editor with a framed box:

- **Enter** submits; **Alt+Enter** or **Ctrl+J** inserts a newline
- **Paste** (bracketed-paste) inserts text verbatim, including newlines
- **Up/Down** navigate history; **Left/Right** move within the line
- **Home/End** jump to line start/end; **Ctrl+U** clears; **Ctrl+C** cancels

Falls back to plain `read_line` when stdin/stdout isn't a TTY.

## Sessions (save / resume)

Every chat is automatically saved to `~/.minion/sessions/` (override with
`MINION_HOME` or `MINION_SESSIONS_DIR`) - one JSON file per session holding
the exact message array the model sees plus a little metadata (id, title,
description, source, cwd, timestamps). Files are plain JSON and
human-readable/greppable.

- **Auto-save** happens after every model turn
- The **title** is auto-derived from your first message; set a custom one with
  `/save <title>`
- A **short id** (the 6-hex suffix) is shown in listings and accepted by
  `--resume` / `/resume`
- **Resume** a session at startup with `minion --resume <target>` or mid-chat
  with `/resume <target>`

## Tools

| tool        | args                  | notes                           |
| ----------- | --------------------- | ------------------------------- |
| `read_file` | `path`                |                                 |
| `write_file`| `path`, `content`     | overwrites; requires confirmation |
| `edit_file` | `path`, `old`, `new`  | `old` must match exactly once   |
| `list_dir`  | `path`                |                                 |
| `run_bash`  | `command`             | requires confirmation           |
| `wait_background` | `pid`          | wait for a backgrounded command |

## License

MIT License. See [LICENSE](LICENSE).
