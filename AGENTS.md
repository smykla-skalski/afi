# AGENTS.md

These instructions apply across the `afi` repository. Direct system, developer, and user instructions take precedence. A deeper `AGENTS.md` overrides this file within its subtree.

## Working rules

- Read the deepest relevant `AGENTS.md` before editing.
- Inspect existing code and call sites before changing behavior.
- Use one dedicated git worktree and one build, test, or runtime lane for the session. Reuse both to keep caches warm.
- Keep edits, generated files, builds, tests, and daemons inside the assigned worktree. Treat other checkouts as read-only unless the user explicitly approves a write.
- Preserve unrelated changes. Commit only explicit task paths, and keep the worktree clean before rebasing.
- If another agent blocks progress for five minutes, ask the user.

## Commands

Run commands from the repository root.

1. Discover available workflows with `mise tasks ls`.
2. Run repository logic with `mise run <task>`.
3. Choose the smallest task that proves the change.

Put environment assignments before `mise`, for example `VAR=value mise run <task>`. Do not wrap `mise` or bypass it with raw `cargo` or `xcodebuild`. If a required workflow is missing, ask the user instead of inventing a substitute command.

## Validation

Match validation to the affected surface:

- Documentation and files outside the codebase: run `git diff --check -- <paths>`.
- Narrow Rust logic: run the focused test task first, then the smallest lint task that covers it.
- Shared CLI, REPL, or model behavior: run the focused test task and the repository test task.
- Dependencies, workflows, or packaging: run `mise run check:supply-chain`. It reads `deny.toml` for the dependency policy and runs actionlint and zizmor over `.github/workflows`.

Run validation in the session worktree. Do not run unrelated gates. Unit tests belong in crate-local `#[test]` blocks; integration tests belong in `tests/`. Isolate filesystem paths, environment variables, ports, and external resource names so tests never require runner-wide serialization. Use `tempfile` for real filesystem state.

## Project map

`afi` is a small coding agent for self-hosted and remote models. It speaks two wire protocols: OpenAI-compatible `/chat/completions` for llama.cpp, vLLM, SGLang, Z.ai, OpenAI, OpenRouter, and Amazon Bedrock, and Anthropic's native Messages API. It renders its interface with raw terminal escapes.

- **Config:** source discovery and switching, per-source protocol and auth mode, provider routing, environment-file loading, and approval state.
- **REPL:** main loop, slash commands, banner, one-shot mode, and automatic session saves.
- **Model:** asynchronous HTTP client, SSE parsing, context-window probing, recovery samplers, context compression, and the tool-dispatch turn loop.
- **Anthropic protocol:** message and tool translation at the client boundary, a stateful SSE decoder normalizing events into the shared chunk type, and the workload-identity-federation token exchange.
- **Bedrock:** AWS SigV4 request signing, credentials and Region read from the standard `AWS_*` variables or assumed from an OIDC identity token through `sts:AssumeRoleWithWebIdentity`, and classification of AWS rejections. Rides the OpenAI-compatible request and SSE paths otherwise.
- **Tools:** file operations, directory listing, detached Bash execution through `setsid`, background waits, protocol parsing, tool-result sanitization, and the allow/deny tool policy enforced both in the request and at dispatch.
- **Sessions:** atomic writes, modification-time ordering, short-ID resolution, and schema versioning.
- **Risk:** command classification, approval gates, Esc-to-chat control flow, and project-root detection.
- **Terminal UI:** Conway's Life spinner, multiline chatbox with bracketed paste, Esc interrupt watcher, and OSC terminal title.
- **CLI:** session listing, resume and session flag resolution, transcript printing, and the `--help` and `--version` short-circuits, the latter carrying build metadata from `build.rs` plus the running executable's own digest.
- **Memory:** developer-memory save, remember, and list operations backed by Markdown files.
- **Metrics:** abbreviated token counts in the statistics footer.
- **Release:** version planning from the commit history, a build matrix that compiles, runs, and packages every target before a tag exists, draft-then-publish ordering, apt and crates.io publication, build provenance, and a reconciliation gate. `scripts/release-targets.sh` is the single definition of what a release contains; `docs/releasing.md` is the operator runbook.

## Rust conventions

- Use Rust 2021.
- Keep Clippy clean under `-D warnings`. Fix warnings instead of adding `#[allow(clippy::...)]` or `#[allow(warnings)]`.
- Keep Rust files under 520 lines and functions under 100 lines.
- Work in small, single-cause chunks: test, implement, then verify.

## Git safety

- Never rebase, amend, or force-push local `main`. Never create merge commits.
- Use Conventional Commit messages: `{type}({scope}): {message}`. Scope is required. Use `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, or `perf` as the type.
- Commit explicit paths with `git commit -sS -- <paths>`. For new files, first run `git add -N -- <paths>`.
- Never use plain `git add`, `git add -A`, `git add .`, `git commit -a`, or `git commit -i`; another agent may have unrelated staged changes.
- Verify each commit with `git log --show-signature -1`. The sign-off must be `Signed-off-by: Bart Smykla <bartek@smykla.com>`.
- Use the configured 1Password SSH signer. Stop if 1Password or the signing key is unavailable. Never bypass signing.

## Debugging

Start with real data. Reproduce the problem with the smallest relevant workflow and preserve useful traces, logs, screenshots, or failure artifacts before changing behavior. Improve observability when evidence is weak, correlate signals across layers, and patch only the proven cause.

## Versioning

Evaluate semantic-version impact for every change. Change versions only with explicit user approval. Documentation-only changes normally need no version bump.

## Platform notes

- Bash execution uses `std::os::unix::process::CommandExt::pre_exec` for `setsid(2)` and does not compile on Windows.
- The chatbox uses crossterm raw mode and falls back to `read_line` when standard input or output is not a TTY.
- Session schema is `afi-1` and is incompatible with earlier formats.
- Environment variables use the `AFI_*` prefix; data lives in `~/.afi/`.
- The legacy tool-call tag uses unusual byte sequences; inspect the protocol module before editing it.
