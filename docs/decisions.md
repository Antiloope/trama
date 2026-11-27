# Decisions

A short log of the decisions that shape Trama, with the reasons behind them.
New decisions go at the end. If one is reversed, add a new entry instead of
editing the old one.

## 1. Rust

**Why:** fast enough for real-time audio, memory safe without a garbage
collector, and its libraries for audio, MIDI, OSC and gamepads work the same
on Linux and macOS. It is also the language we want to learn.

## 2. GPL-3.0-or-later from day one

**Why:** Trama is meant to be free software that stays free. Every
dependency must be GPL-3.0 compatible, checked by `cargo deny`.

## 3. Our own engine, not SuperCollider or a DAW

**Why:** writing the engine is the point of the project, and Rust has solid
building blocks (cpal, fundsp). SuperCollider's `scsynth` with its
VSTPlugin extension stays a fallback for hosting plugins if doing it
ourselves gets too hard, which the message design would let us plug in
without rewriting the rest.

## 4. Plugins: CLAP first, then VST3, no AU for now

**Why:** CLAP is simpler to host and many good free plugins support it.
VST3 covers almost everything else on both systems. AU hosting in Rust is
immature, and Mac-only plugins usually ship as VST3 too.

## 5. A web page as the user interface

**Why:** the same page works on the computer, the iPad and the phone with
nothing to install, and treating the UI as one more device keeps Trama usable
as a black box with no screen.

## 6. Setups are files

**Why:** building and tearing down setups should be cheap. A setup is data
(gear, wiring, mappings), not code.

## 7. The audio engine is the master clock

**Why:** the sound card is the only clock that never drifts relative to the
audio. Anything on the beat is decided inside the engine, in samples.

## 8. Synced and free loops

**Why:** beats need loops locked to the tempo; ambient textures benefit from
loops of any length that drift against each other. The engine supports both
from the start.

## 9. Code and docs in English

**Why:** it is the common language of Rust and open source, so anyone can
read and contribute.

## 10. Mappings also decide the lights

**Why:** if the core's mappings say how each control should be lit (as
`Notification`s addressed to one device), adapters stay simple translators
and the meaning of the grid lives in one place. The Launchpad adapter knows
how to light pad (4, 5) green, not why.

## 11. MIDI timing through anchors

**Why:** the engine places MIDI on exact samples, but MIDI ports only send
"now". The engine leaves an anchor (sample, instant) every block, and the
MIDI output maps samples to instants using the smallest recent offset,
because audio callbacks are only ever late. Messages go out 10 ms after
their sample's time so the MIDI thread is never the one making them late.
Live controls such as pitch bend skip the queue and go out at once.

## 12. The first mapping is code; setup files come later

**Why:** milestone 1 needs one layout (the step sequencer), and writing it in
code first shows what a mapping must be able to say before designing the
file format (milestone 5).

## 13. cpal's native PipeWire backend on Linux

**Why:** PipeWire is what Fedora runs, and talking to it directly avoids the
extra ALSA layer. cpal falls back to ALSA when PipeWire is not running. It
needs `pipewire-devel` and `clang-devel` to build.
