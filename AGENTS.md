# Working on Trama

Trama is a Rust workspace, for Linux and macOS, that connects synths, MIDI
controllers and gamepads to a real-time engine. `docs/architecture.md`
explains how the crates fit together; `CONTRIBUTING.md` has every
convention in full.

## Before you finish

Run these and fix what they report:

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `scripts/check-docs.sh`

## Documentation

The repository describes Trama as it is today. Plans live in GitHub issues.

- Write nothing about future work in code, comments, the README or `docs/`:
  no project phases, no lists of upcoming features, no wording that marks
  something as temporary. If something is missing, open or update an issue
  (`gh issue create`, `gh issue comment`) instead.
- Leave no to-do or fix-me comments. Open an issue.
- Never point at issues, plans or numbered document sections from code.
- Prefer names that explain themselves. Comment only what the code cannot
  say:
  - `//!` at the top of a crate or module: what it is responsible for and
    its rules, in a few lines;
  - `///` on public items: only what the signature does not say (units,
    ranges, edge cases, invariants, real-time constraints), never the name
    restated;
  - `//` inside a function: only a non-obvious *why*, never *what*.
- When code changes, update the README and `docs/` in the same change.
- `docs/decisions.md` records decisions the code already reflects, in the
  present tense. Decisions about work not done go in that work's issue.

`scripts/check-docs.sh` catches the words these rules forbid, and a Claude
Code hook runs it after every edit and before you finish. It cannot judge
whether a comment is needed: that part is on you.

## Code

- Code, comments, docs, commit messages and issues are in English.
- Every source file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`.
- Dependencies must be GPL-3.0 compatible and `cargo deny check` must pass.
  Do not widen `deny.toml` without checking the license.
- No `unsafe`: the workspace lints forbid it.
- Crate dependencies point one way: `trama` uses `trama-io`, `trama-core`
  and `trama-engine`, which use `trama-protocol`. `trama-core` knows no
  devices or audio, `trama-engine` knows no devices or sound card, and
  `trama-io` knows neither the core nor the engine.
- `EngineProcessor` runs on the audio thread: it must never allocate, lock,
  block, print or touch files. `crates/trama-engine/tests/realtime.rs`
  checks allocations.
