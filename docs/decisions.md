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
