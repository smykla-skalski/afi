# Changelog

All notable changes to `afi`. Generated from the commit history; maintained by
release-plz from the conventional-commit messages on `main`.

## [0.6.0] - 2026-08-07

### Added

- **release** Build targets before publishing ([#65](https://github.com/smykla-skalski/afi/pull/65))
- **prompt** Let a run supply its own prompt ([#67](https://github.com/smykla-skalski/afi/pull/67))

### Fixed

- **model** Keep credentials out of reported error bodies ([#66](https://github.com/smykla-skalski/afi/pull/66))

## [0.5.0] - 2026-08-07

### Added

- **summary** Report the credential a run billed ([#53](https://github.com/smykla-skalski/afi/pull/53))
- **summary** Report refused tool calls ([#56](https://github.com/smykla-skalski/afi/pull/56))
- **summary** Classify why a run failed ([#54](https://github.com/smykla-skalski/afi/pull/54))
- **config** Set reasoning effort with one flag ([#55](https://github.com/smykla-skalski/afi/pull/55))

### Dependencies

- **deps** Lock file maintenance ([#57](https://github.com/smykla-skalski/afi/pull/57))

## [0.4.0] - 2026-08-06

### Added

- **summary** Write the run summary to a file ([#52](https://github.com/smykla-skalski/afi/pull/52))

## [0.3.0] - 2026-08-06

### Added

- **tools** Add a read-only posture ([#41](https://github.com/smykla-skalski/afi/pull/41))
- **anthropic** Let thinking be turned on ([#36](https://github.com/smykla-skalski/afi/pull/36))
- **release** Automate version and changelog ([#37](https://github.com/smykla-skalski/afi/pull/37))
- **cli** Add --version and --help ([#38](https://github.com/smykla-skalski/afi/pull/38))

### Fixed

- **version** Hash without digest's io::Write impl ([#40](https://github.com/smykla-skalski/afi/pull/40))

## [0.2.0] - 2026-08-06

### Added

- **packaging** Publish debs to an apt repo (#32)
- **summary** Report cost from token rates (#33)
- **tools** Tool allow/deny lists + CI gate (#28)
- **model** Break out cache-write tokens (#22)
- **summary** Report runs as JSON, exit non-zero (#18)

### Dependencies

- **deps** Update rust crate tui-markdown to v0.3.9 (#26)
- **deps** Update rust crate tokio-util to v0.7.19 (#25)
- **deps** Update rust crate tokio to v1.53.1 (#24)
- **deps** Update rust crate serde_json to v1.0.151 (#13)
- **deps** Update rust crate thiserror to v2.0.19 (#14)
- **deps** Update rust crate serde to v1.0.229 (#11)
- **deps** Pin rust crate tempfile to =3.27.0 (#10)
- **deps** Update rust crate async-trait to v0.1.91 (#2)

## [0.1.0] - 2026-08-06

### Added

- **model** Add native Anthropic Messages API (#9)
- **repl** Add natural arrow-key history
- **repl** Add conversation scrolling
- **model** Wire spinner, Esc, real approval
- **term** Migrate chatbox input to Ratatui
- **model** Connect model turn loop to stream tokens and dispatch tools
- **repl** Add main REPL loop, slash commands, and one-shot mode
- **term** Add TUI shell, chatbox input, and Esc interrupt watcher
- **model** Add recovery samplers and context compression
- **model** Add HTTP client, SSE streaming, and context window probing
- **risk** Add risk classifier, approval gating, memory, and metrics
- **tools** Add file tools, bash execution, and text-protocol parser
- **sessions** Add session persistence with atomic writes and listings
- **core** Scaffold project with env loading, config, and approval gating

### Changed

- **repl** Replace terminal UI with Ratatui
- **lint** Strict clippy parity + 420-line source-size remediation
- **branding** Remove legacy Minion references
- **bash** Replace unsafe pre_exec setsid with safe process_group(0)
- Rename all env vars and data dir from MINION_ to AFI_

### Dependencies

- **deps** Update rust crate futures to v0.3.33 (#3)
- **deps** Modernize direct dependencies and drop unused crates

### Documentation

- **readme** Move reference tables to docs/, credit minion.py
- **readme** Tighten prose, refresh screenshot
- **agents** Streamline agent instructions
- Finalize README with full configuration reference

### Fixed

- **repl** Preserve multiline composer input

### Performance

- **repl** Optimize Ratatui rendering
