# Decisions

The decisions that shape Trama as it is, and the reasons behind them. Each
entry describes something the code reflects. When a decision changes, its
entry is rewritten to say what holds now.

## Rust

Fast enough for real-time audio, memory safe without a garbage collector,
and its libraries for audio, MIDI and gamepads work the same on Linux and
macOS. It is also the language the project is meant to teach its authors.

## GPL-3.0-or-later

Trama is free software that stays free. Every dependency must be GPL-3.0
compatible, which `cargo deny check` verifies against `deny.toml`.

## An engine of its own

Trama has its own engine instead of building on SuperCollider or a DAW:
writing it is the point of the project, and Rust has the building blocks
(cpal for the sound card, rtrb for lock-free queues).

## The sound card is the master clock

The sound card is the only clock that never drifts relative to the audio.
Everything on the beat is decided inside the engine, counted in samples.

## Mappings decide the lights

The core's mapping says how each control should be lit, as `Notification`s
addressed to one device. Adapters stay simple translators, and the meaning
of the grid lives in one place: the Launchpad adapter knows how to light
pad (3, 4) blue, not why.

## MIDI timing through anchors

The engine places MIDI on exact samples, but MIDI ports only send "now". The
engine leaves an anchor (sample, instant) every block, and the synth output
maps samples to instants using the smallest recent offset, because audio
callbacks are only ever late. Messages go out 10 ms after their sample's
time, so the MIDI thread is never the one making them late. Live controls
such as pitch bend skip the queue and go out at once.

## The mapping is code

There is one layout, the step sequencer, so it is written in code. That
keeps it simple and shows exactly what a mapping needs to express.

## cpal's native PipeWire backend on Linux

PipeWire is what Fedora runs, and talking to it directly avoids an extra
ALSA layer. cpal falls back to ALSA when PipeWire is not running. Building it
needs `pipewire-devel` and `clang-devel`.

## Code and docs in English

It is the common language of Rust and open source, so anyone can read and
contribute.

## The repository describes the present

Code, comments and docs say what Trama is, not what it will be. Plans live
in GitHub issues, where they can change without leaving stale text behind.
`scripts/check-docs.sh` and the Claude Code hooks in `.claude/settings.json`
check the words that usually break this rule.
