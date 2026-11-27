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

## Devices are described by kind, not by model

Trama works with whatever gear its user has. Only an adapter in `trama-io`
knows a particular model: it names that model's controls by kind (a pad on
a grid, the button beside a row, a gamepad's `south` button) and sends
`Input`s in those terms. The protocol, the core and the engine never name a
model, so a mapping works with every device of its kind, and supporting a
new model means writing its adapter and nothing else. Defaults never assume
a particular device: with no options, `trama play` creates a virtual MIDI
port for the synth and uses a grid controller only if one is connected.

## The mapping is code

There is one layout, the step sequencer, so it is written in code. That
keeps it simple and shows exactly what a mapping needs to express.

## The audio graph is a patchbay

Audio goes through nodes connected by cables, the way gear is connected
with a patchbay, so anything that makes or changes sound plugs into the
same place. Each cable carries one channel: a microphone is one cable and
a stereo synth two, and nothing assumes a channel count. Cables that meet
at a jack are summed, as on a mixer. Loops are refused, because a graph
without loops has an order to run its nodes in.

The patch is a `GraphDescription` in `trama-protocol`, plain data that any
layer can write and that knows nothing about who wrote it. The engine
prepares it into a graph it can run.

## Graphs are swapped, never built, on the audio thread

Building a graph allocates, and the audio thread must never allocate or
free. The controller prepares each graph on its own thread and hands it
over; the processor swaps it in and hands the old one back to be freed.
Graphs travel in the same queue as the commands, so everything reaches the
processor in the order it was sent.

## Level changes glide over a fixed time

A gain that jumps clicks. Every change of level, mute included, glides
over 10 ms. A fixed time rather than one block keeps the sound the same
whatever the block size, which can be under a millisecond or near a
hundred.

## cpal's native PipeWire backend on Linux

PipeWire is the sound server of most current Linux desktops, and talking
to it directly avoids an extra ALSA layer. cpal falls back to ALSA when
PipeWire is not running. Building it needs PipeWire's development files and
libclang (the README lists the packages).

## Code and docs in English

It is the common language of Rust and open source, so anyone can read and
contribute.

## The repository describes the present

Code, comments and docs say what Trama is, not what it will be. Plans live
in GitHub issues, where they can change without leaving stale text behind.
`scripts/check-docs.sh` and the Claude Code hooks in `.claude/settings.json`
check the words that usually break this rule.
