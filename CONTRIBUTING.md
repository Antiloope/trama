# Contributing to Trama

## Setting up

1. Install Rust with [rustup](https://rustup.rs). `rust-toolchain.toml`
   picks the latest stable release with `rustfmt` and `clippy`.
2. Install the system libraries: ALSA for MIDI, PipeWire for audio,
   libclang to generate PipeWire's Rust bindings, libudev for gamepads.
   - Debian and Ubuntu: `sudo apt install pkg-config libasound2-dev libpipewire-0.3-dev libclang-dev libudev-dev`
     (what CI installs);
   - Fedora: `sudo dnf install alsa-lib-devel pipewire-devel clang-devel systemd-devel`;
   - macOS: `xcode-select --install`.

## Everyday commands

| Command                                     | What it does                               |
|---------------------------------------------|--------------------------------------------|
| `cargo run -- doctor`                       | list the audio outputs and inputs, MIDI ports and gamepads Trama finds |
| `cargo run --release`                       | run the server, in this terminal           |
| `cargo run -- status`                       | ask the running server what it has open    |
| `cargo run --release -- play`               | play, with an optimized build              |
| `cargo test --workspace`                    | every test in every crate                  |
| `cargo fmt --all`                           | format the code                            |
| `cargo clippy --workspace --all-targets -- -D warnings` | lint                           |
| `cargo deny check`                          | check every dependency's license (install it with `cargo install cargo-deny`) |
| `scripts/check-docs.sh`                     | check the documentation rules below        |

Debug builds are partly optimized (see `[profile.dev]` in `Cargo.toml`),
because audio code without optimizations can be too slow to keep time.
`Cargo.lock` is committed so both systems build with the same library
versions.

## Tests

From fastest to slowest:

1. **Unit tests**, next to the code they test: names, MIDI encoding,
   scales and note names, zones, row buttons and transforms, Launchpad
   bytes, gamepad events, the clock map, the cushion between the audio
   input and output, its round trip and the loop that steers its
   resampler, the patch `trama play` builds and how it shares the grid,
   the server's lock and how it lets go of a client that falls behind.
   `crates/trama-io/src/audio/clocks_apart.rs` runs an input and an output
   offline on clocks that drift apart, for minutes, and checks that the
   cushion stays put and a sine comes out clean.
2. **The core as a black box** (`crates/trama-core/tests`): devices,
   modules and connections set up, inputs in, engine commands,
   notifications and the engine patch out. `step_sequencer.rs` connects the
   sequencer as `trama play` does; `looper.rs` presses clips, scenes, stop
   and delete and feeds back what the engine reports; `patch.rs` checks
   what connections accept, refuse and carry.
3. **The engine offline** (`crates/trama-engine/tests`): it runs without a
   sound card. `offline.rs` checks which MIDI comes out on which sample,
   for several block sizes and while patches are swapped; `graph.rs` feeds
   the audio graph synthetic input and checks exactly what comes out;
   `looper.rs` records loops and checks on which sample they start, what
   they play back, at which speed, and what a full memory, a stopped clock,
   the latency and a swap do to them.
4. **The server as a black box** (`crates/trama/tests/server.rs`): it
   starts inside the test with no sound card, on a free port and a
   temporary user data folder, and is driven through its API. Two
   WebSocket clients see the same change, a control a module does not have
   is refused with its reason, requests from other sites are refused, and
   a second server says where the first one is.
5. **Real-time safety** (`crates/trama-engine/tests/realtime.rs`): the
   processor runs under `assert_no_alloc`, which aborts the test if the
   audio path allocates, with step players, gains and loopers recording,
   playing and being swapped. `clocks_apart.rs` does the same for the input's
   resampler and the cushion.
6. **End to end, without hardware**: `cargo run -p trama-io --example
   fake_gear` creates a pretend Launchpad and synth on virtual MIDI ports
   (Linux and macOS). Point `trama play` at them (`--pads "Fake LPX" --synth
   "Fake Synth" --no-gamepad`) and it presses pads, plays, stops, and
   reports the notes the synth got and how steady the MIDI clock was.
7. **With the gear**: `trama doctor`, then `trama play` with the real
   devices.

CI (`.github/workflows/ci.yml`) runs formatting, clippy and tests 1 to 5 on
Linux and macOS for every push, and checks licenses. Tests 6 and 7 need a
MIDI system and hardware, so they are run by hand.

## Documentation

The repository describes Trama as it is today. Plans, ideas, bugs and
things to try live in [GitHub issues](https://github.com/Antiloope/trama/issues),
labelled `enhancement`, `bug` or `testing`.

| Where                       | What goes there                                                         | What does not                          |
|-----------------------------|-------------------------------------------------------------------------|----------------------------------------|
| Names in the code           | The first documentation: types, functions and variables that explain themselves | —                              |
| `//!` on a crate or module  | What it is responsible for and its rules, in a few lines                | History, links to document sections    |
| `///` on public items       | What the signature cannot say: units, ranges, edge cases, invariants    | The name restated                      |
| `//` inside a function      | A non-obvious *why*                                                     | *What* the code does, to-do notes, plans |
| `README.md`                 | What Trama is, requirements, how to build, run and play                 | Project status, phases                 |
| `CONTRIBUTING.md`           | How to work on the repository                                           | —                                      |
| `docs/architecture.md`      | How Trama is built: layers, messages, time, threads, crates             | Features that do not exist             |
| `docs/decisions.md`         | Why it is built this way: decisions the code reflects, in the present tense | Decisions about work not done      |
| GitHub issues               | Plans, ideas, bugs, things to try                                       | —                                      |

A few more rules:

- No to-do or fix-me comments: open an issue.
- Code never points at issues, plans or numbered document sections.
- A change that alters behaviour updates the README and `docs/` too.

`scripts/check-docs.sh` looks for the words that usually break these rules
(project phases, reminders, section numbers) in every tracked file. Run it
before opening a pull request. Claude Code runs it automatically: the hooks
in `.claude/settings.json` check each file an agent edits, and every changed
file before the agent finishes. Agents read the same rules from `AGENTS.md`
(`CLAUDE.md` imports it).

## Code conventions

- Code, comments, docs, commit messages and issues are in English.
- Every source file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`.
- Trama is GPL-3.0-or-later, so every dependency must be compatible (MIT,
  Apache-2.0, BSD, ISC, Zlib, MPL-2.0, LGPL, GPL-2.0-or-later, ...).
  `cargo deny check` enforces the list in `deny.toml`; check a new license
  before adding it there.
- No `unsafe`: the workspace lints forbid it.
- Crates depend on each other only as `docs/architecture.md` describes.
- Trama works with whatever gear its user has:
  - code that knows a particular model of device lives only in that model's
    adapter in `trama-io`;
  - the protocol, the core and the engine talk about kinds of controls
    (pads, row buttons, arrows, gamepad buttons and sticks), never about
    models;
  - defaults never assume a particular device;
  - code, comments and docs name a model only where Trama has an adapter
    for it; examples use made-up names ("My Synth").
- Nothing on the audio thread may allocate, lock, block, print or touch
  files.

## Changes

- One branch and one pull request per change.
- The pull request says what changed and how it was tested, including what
  could not be tested (for example, hardware that was not connected).
- CI must pass before merging.
