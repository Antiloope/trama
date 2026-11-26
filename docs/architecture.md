# Trama architecture

This document explains how Trama is put together, from the big picture down
to the packages, the build and the tests. It is meant to be read top to
bottom. Anything marked *(planned)* does not exist in the code yet, and the
sketches of types and files will change as we build them.

Decisions and the reasons behind them are logged in
[decisions.md](decisions.md).

## 1. The big picture

Trama is a program that runs on a computer (Linux or macOS) and turns a pile
of instruments and controllers into one instrument.

You plug in your gear. Trama listens to all of it, decides what each gesture
means according to the current **setup**, makes sound, records, and lights up
the controllers to show what is going on. A web page, opened on the computer,
a tablet or a phone, is used to configure things. Once a setup is ready you
can play without looking at a screen.

Trama does three jobs, and each job is a layer:

| Layer      | Job                                                           | In one word |
|------------|---------------------------------------------------------------|-------------|
| **IO**     | Talk to the outside world: devices, sound card, network, disk | translate   |
| **Core**   | Know the state of the session and decide what happens         | decide      |
| **Engine** | Make the sound, on time, every few milliseconds               | play        |

A **setup** is not code: it is a file that tells the core which gear exists
and what each control does.

```
   ┌──────────────────────────────────────────────────────────────┐
   │  OUTSIDE WORLD                                               │
   │  Launchpad X · XPS-10 · OP-1 · MiniBrute 2S · gamepad        │
   │  iPad / phone (web page) · other computer · files on disk    │
   └───────────────┬──────────────────────────────▲───────────────┘
                   │ bytes, presses, packets      │ LEDs, MIDI, web updates
   ┌───────────────▼──────────────────────────────┴───────────────┐
   │  IO       adapters, one per kind of device                   │
   └───────────────┬──────────────────────────────▲───────────────┘
                   │ Input                        │ Notification
   ┌───────────────▼──────────────────────────────┴───────────────┐
   │  CORE     session state + mappings       ◄──── setup file    │
   └───────────────┬──────────────────────────────▲───────────────┘
                   │ EngineCommand                │ EngineEvent
   ┌───────────────▼──────────────────────────────┴───────────────┐
   │  ENGINE   audio graph, clock, loopers, synths, mixer         │
   └───────────────┬──────────────────────────────▲───────────────┘
                   │ audio out                    │ audio in
   ┌───────────────▼──────────────────────────────┴───────────────┐
   │  OPERATING SYSTEM   PipeWire / JACK (Linux), CoreAudio (macOS)│
   │                     → Scarlett 2i2 → speakers, headphones    │
   └──────────────────────────────────────────────────────────────┘
```

The piece of IO that opens the sound card is tiny: it only hands each block
of audio to the engine and back. Audio never passes through the core.

Three rules hold everything together:

1. **Everything is a message.** Layers never reach into each other; they send
   messages (section 2).
2. **Each piece of state has one owner.** The core owns the session, the
   engine owns what is sounding right now. Everyone else asks (section 3).
3. **The audio thread never waits.** Anything slow happens elsewhere and is
   handed over ready to use (section 5).

## 2. Messages

There are five kinds of messages. Their types live in `trama-protocol`.

| Message         | Meaning                           | Example                                         |
|-----------------|-----------------------------------|-------------------------------------------------|
| `Input`         | what happened on a device         | pad (4, 5) on `pads` pressed, velocity 0.79     |
| `Command`       | what the musician wants           | launch clip (track 5, scene 4)                  |
| `EngineCommand` | exactly what the engine must do   | start node 12 on the next bar                   |
| `EngineEvent`   | what the engine reports back      | node 12 started at sample 1 536 000             |
| `Notification`  | what changed in the session       | clip (5, 4) is now playing                      |

`Input` is about devices ("this button"). `Command` is about music ("this
clip"). The **mappings** in the setup file are the bridge between the two,
which is why changing a setup never needs new code.

`Command`s can also arrive directly, without an `Input`: the web page and
other computers send them.

### Walk-through: pressing a pad on the Launchpad

1. The Launchpad sends three bytes over USB: `90 2D 64` (note on, note 45,
   velocity 100).
2. The Launchpad adapter (IO) knows its layout and produces an `Input`:
   pad at row 4, column 5, pressed with velocity 0.79.
3. The core looks at the setup's mappings. In this setup the grid means
   "column = track, row = scene", so the `Input` becomes the `Command`
   *launch clip (track 5, scene 4)*.
4. The core updates the session: clip (5, 4) is now *queued*. It sends a
   `Notification`; the Launchpad adapter makes that pad blink and the web page
   updates.
5. The core sends the engine an `EngineCommand`: *start node 12 on the next
   bar*.
6. Inside the audio thread, the engine starts the clip exactly on the first
   sample of the next bar and reports an `EngineEvent`.
7. The core marks the clip as *playing* and sends a `Notification`; the pad
   turns solid green.

From press to blinking light takes a millisecond or two. The sound itself
waits for the bar line on purpose.

### Why messages

- **Setups are just data**: a mapping is "this `Input` → that `Command`".
- **Presets and scenes are snapshots** of the state the messages built.
- **Recording a performance** means saving the `Command`s with their times;
  replaying them reproduces the jam, even with different sounds.
- **The network is free**: messages can be serialized and sent to another
  computer, or come from a phone.
- **Tests are easy**: send messages in, check what comes out.

## 3. State: who owns what

| Owner        | What it owns                                                                       |
|--------------|------------------------------------------------------------------------------------|
| Core         | The session: tracks, clips, scenes, instruments, parameter values, tempo, mappings |
| Engine       | What is sounding now: playhead positions, filter memory, buffers being recorded    |
| IO adapters  | Only what they need to talk to their device, such as which LEDs are lit            |
| Disk         | Setups, sessions, samples, recordings                                              |

The core is the single source of truth. If the web page wants to change a
parameter, it sends a `Command` to the core like any other device.

## 4. Time

**The sound card is the clock.** At 48 kHz with blocks of 128 samples, it asks
for audio every 2.7 ms. The engine counts samples, and that count is Trama's
master clock. Bars and beats are computed from the count and the tempo.

- **Everything that must land on the beat is decided inside the engine**,
  in samples: clip launches, loop boundaries, step sequencers, the MIDI clock
  sent to hardware.
- **The core thinks ahead.** It says "on the next bar", not "now".
- **MIDI to hardware** (MiniBrute 2S, OP-1, XPS-10): the engine writes
  time-stamped MIDI events into a queue and a MIDI-out thread in IO sends
  them. This is the one path that goes from the engine straight to IO without
  passing through the core, because timing matters more than anything else
  there.
- **Ableton Link** *(planned)*: the engine nudges its tempo and beat phase to
  match other computers and apps on the network.

Two kinds of loops, both supported by the engine from the start:

- **synced loops** have a length in bars and follow the tempo (beats, rave);
- **free loops** have a length in samples and ignore the tempo, so loops of
  different lengths drift against each other (ambient, textures).

## 5. Threads

Trama runs several threads (things happening at the same time):

```
  ┌──────────────────────────────────────────┐
  │ audio thread (real-time)                 │  engine processor
  └──────────▲──────────────────┬────────────┘
             │  lock-free queues: fixed size, never block
  ┌──────────┴──────────────────▼────────────┐
  │ core thread                              │  session, mappings
  └──────────▲──────────────────┬────────────┘
             │  channels
  ┌──────────┴──────────────────▼────────────┐
  │ IO threads                               │  MIDI in/out, gamepad,
  │                                          │  network, disk
  └──────────────────────────────────────────┘
```

### The audio thread never waits

The audio thread is like a drummer who can never miss a beat. If it is late
even once, you hear a click. So inside it Trama **never**:

- allocates or frees memory;
- takes a lock (waits for another thread);
- touches files or the network;
- prints, logs, or calls anything that might do one of the above.

Heavy work is done **outside and handed over ready to use**. Loading a sample
means: the disk thread decodes the file, the engine controller wraps it in a
sampler node, and the audio thread receives the finished node through a
queue and plugs it in. Whatever the audio thread stops using is sent back out
to be freed elsewhere.

That is why the engine has two halves: the **controller** (normal thread, may
allocate) and the **processor** (audio thread, may not).

Tests enforce the rule: the processor runs under `assert_no_alloc`, which
makes a test fail if the audio path allocates (section 10).

## 6. Packages

Trama is a Cargo **workspace**: one repository with several packages
(*crates*) that are built and tested together.

```
trama/
├── Cargo.toml            the workspace: list of crates and shared settings
├── crates/
│   ├── trama-protocol/   the messages every layer speaks
│   ├── trama-dsp/        pure audio building blocks (math on buffers)
│   ├── trama-engine/     the real-time engine (no devices, no sound card)
│   ├── trama-core/       the brain: session state, setups, mappings, scenes
│   ├── trama-io/         adapters: sound card, MIDI, gamepad, OSC, web, disk
│   └── trama/            the program: wires everything together
├── docs/                 this document and the decision log
├── setups/               example setup files (planned)
└── web/                  the web interface (planned)
```

Who may depend on whom:

```
                    trama  (the program)
           ┌──────────┼──────────────┐
           ▼          ▼              ▼
       trama-io   trama-core    trama-engine
           │          │           │       │
           │          │           │       ▼
           │          │           │   trama-dsp
           ▼          ▼           ▼
                 trama-protocol
```

- **Arrows only point down.** Nothing depends on the program, and
  `trama-protocol` and `trama-dsp` depend on no other Trama crate.
- **`trama-core` knows nothing about devices or audio.** It is pure logic.
- **`trama-engine` knows nothing about devices or the sound card.** It is
  "audio in, audio out", so tests can run it offline and render to a file.
- **`trama-io` knows nothing about the core or the engine.** It translates.
- **Only `trama` knows everyone**, and its job is to connect them.
- **No crate may use `unsafe`** (enforced by the compiler through the
  workspace lints). The future `trama-plugins` crate will be the one
  exception, because talking to C/C++ plugins requires it, and it keeps that
  code in one place.

Splitting into crates makes the compiler enforce these boundaries: the engine
cannot call the MIDI library by accident because it does not depend on it.
It also makes rebuilds faster, since only the crates that changed recompile.

An internal dependency is added to a crate's `Cargo.toml` the first time code
uses it (Cargo warns about unused dependencies), always following the
diagram above.

Planned crates:

- `trama-plugins`: hosting CLAP plugins first, VST3 later.

## 7. Setups, sessions and presets

- A **setup** says which gear is connected, how audio is wired and what
  controls what. It is a TOML file in `setups/`. Building or tearing down a
  setup means loading another file.
- A **session** is what you make with a setup: recorded loops, clips, scenes,
  parameter values. It is saved as a folder (a session file plus audio files).
- A **preset** is saved settings for one instrument or effect.

A first sketch of a setup file (the format will change):

```toml
name = "studio"

[audio]
device = "Scarlett 2i2"
sample_rate = 48000
block_size = 128

[devices.pads]
kind = "launchpad-x"

[devices.mono]
kind = "midi"
port = "MiniBrute 2S"

[devices.joy]
kind = "gamepad"

[tracks.drone]
source = { audio_input = 1 }

[[mappings]]
from = "joy.left_stick.x"
to = "tracks.drone.filter.cutoff"
range = [200.0, 8000.0]
curve = "exponential"
```

User data (samples, recordings, sessions) lives in the standard place for
each system: `~/.local/share/trama` on Linux and
`~/Library/Application Support/trama` on macOS.

## 8. User interface

- Trama serves its own **web page** *(planned)*. Open it on the computer, the
  iPad or the phone.
- The page is **just another device**: it sends `Command`s and receives
  `Notification`s, exactly like the Launchpad.
- It has two modes: **configure** (devices, plugins, mappings, setups) and
  **play** (mixer, macros, scenes).
- **Black-box mode**: with no page open, everything keeps working from the
  controllers.
- The page is plain HTML, CSS and JavaScript embedded in the `trama` binary,
  with no JavaScript build step, at least at first.
- The only native windows will be plugin editors, which bring their own UI.

## 9. Building

[Cargo](https://doc.rust-lang.org/cargo/) is Rust's build tool and package
manager. Everything goes through it:

| Command                   | What it does                                    |
|---------------------------|-------------------------------------------------|
| `cargo run`               | build and start `trama`                         |
| `cargo run --release`     | optimized build; use it to actually play        |
| `cargo test`              | run every test in every crate                   |
| `cargo fmt`               | format the code                                 |
| `cargo clippy`            | lint: catches common mistakes                   |
| `cargo deny check`        | check every dependency's license (section 11)   |

- **One workspace, one `Cargo.lock`, one `target/` folder.** `Cargo.lock` is
  committed: Trama is an application, and both computers should build with
  exactly the same library versions.
- **`rust-toolchain.toml`** makes every machine use the latest stable Rust
  with `rustfmt` and `clippy`.
- **Debug builds are partly optimized** (see `[profile.dev]` in
  `Cargo.toml`), because unoptimized audio code can be too slow to sound
  right.
- **Each computer builds for itself.** No cross-compiling: Linux builds on
  Linux, macOS builds on macOS.
- **CI** (`.github/workflows/ci.yml`) builds, lints and tests on Linux and
  macOS on every push, so "works on both" is checked by a machine. It runs
  once the repository is on GitHub.

System libraries, needed once audio, MIDI and gamepad code lands:

- Fedora: `sudo dnf install alsa-lib-devel pipewire-devel clang-devel systemd-devel`
- macOS: `xcode-select --install`

macOS asks for **microphone permission** before any program can record from
an audio input, including the Scarlett. The first time Trama records, allow
it for the terminal you run it from.

## 10. Testing

From fastest to slowest:

1. **Unit tests**, next to the code they test (`#[cfg(test)]` modules). DSP
   math, core logic, message parsing. They run in milliseconds and need no
   hardware.
2. **Offline engine tests.** The engine runs without a sound card: send it
   `EngineCommand`s, call its process step for N blocks, check the samples.
   For example "the clip starts exactly at sample 96 000", "this loop is 4
   bars long", "no NaNs". The same tests can write WAV files to listen to.
3. **Snapshot tests.** Render a short scene and compare it with a stored
   reference recording, within a tolerance, to catch unintended changes in
   the sound.
4. **Real-time safety tests.** Run the engine processor under
   `assert_no_alloc`; the test fails if the audio path allocates.
5. **Adapter tests.** Byte sequences recorded from the real devices
   (fixtures) go into an adapter, and we check the `Input`s that come out.
   Virtual MIDI ports, available on Linux and macOS, let us test the whole
   chain without hardware.
6. **Hardware smoke tests.** A short manual checklist per device, helped by
   a planned `trama doctor` command that lists the audio devices, MIDI ports,
   gamepads and network address it finds.

CI runs 1 to 5 on Linux and macOS. Number 6 is you, with the gear.

## 11. License

- Trama is **GPL-3.0-or-later**. The full text is in `LICENSE`, every
  `Cargo.toml` declares it, and every source file starts with
  `// SPDX-License-Identifier: GPL-3.0-or-later`.
- **Every dependency must be GPL-3.0 compatible** (MIT, Apache-2.0, BSD, ISC,
  Zlib, MPL-2.0, LGPL, GPL-2.0-or-later, ...). `cargo deny check` enforces the
  list in `deny.toml`, and CI runs it.
- Known future dependencies are fine: Ableton Link is GPL-2.0-or-later, and
  the CLAP and VST3 SDKs are MIT.
- Plugins are separate programs that Trama loads through their standard
  interfaces, the same way other GPL hosts such as Ardour do. They are not
  part of Trama's code.

## 12. Roadmap

Each milestone is playable on its own.

0. **Skeleton**: this workspace, the docs, CI. *(done)*
1. **Gear without synthesis**: open the sound card and run an engine whose
   only job is keeping time. The Launchpad becomes a step sequencer, the
   gamepad adds pitch bend, and both drive the MiniBrute 2S over MIDI with
   MIDI clock. This exercises IO, core, protocol and the clock.
2. **First sounds**: a simple synth in `trama-dsp`, the engine playing it,
   offline tests that render to WAV.
3. **Looper**: record the Scarlett inputs into synced and free loops, with the
   Launchpad as an 8 × 8 grid of tracks and scenes.
4. **Samples and disk**: sampler, importing the OP-1's tracks from its disk
   mode, an always-on recorder that keeps the last few minutes.
5. **Web interface and setups**: the configure and play pages, setup files.
6. **Plugins**: CLAP first, then VST3.
7. **Network**: Ableton Link, two computers, playing as a duo.
