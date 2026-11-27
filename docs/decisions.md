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

## Modules decide the lights, connections say which

A module says how each of its light outputs lights a control, and a
connection from that output to a control, or a zone of pads, says which
control shows it; the core sends that as `Notification`s addressed to one
device. Adapters stay simple translators, and the meaning of the grid lives
in the module: the Launchpad adapter knows how to light pad (3, 4) blue, not
why.

A control may feed several modules, but only one connection lights it. Two
modules lighting one button would make it flicker between them, and the
musician could not tell which one it shows.

## MIDI timing through anchors

The engine places MIDI on exact samples, but MIDI ports only send "now". The
engine leaves an anchor (sample, instant) every block, and the synth outputs
map samples to instants using the smallest recent offset, because audio
callbacks are only ever late. Messages go out 10 ms after their sample's
time, so the MIDI thread is never the one making them late. Live controls
such as pitch bend skip the queue and go out at once.

## Devices are described by kind, not by model

Trama works with whatever gear its user has. Only an adapter in `trama-io`
knows a particular model: it names that model's controls by kind (a pad on
a grid, the button beside a row, a gamepad's `south` button) and sends
`Input`s in those terms. The protocol, the core and the engine never name a
model, so a connection works with every device of its kind, and supporting
a new model means writing its adapter and nothing else. Defaults never assume
a particular device: with no options, `trama play` creates a virtual MIDI
port for the synth and uses a grid controller only if one is connected.

## Modules and connections, not layouts

Trama does not have a layout for each kind of device. A module does one
job and declares its inputs and outputs; connections say which control of
which device goes to which input, and which output goes to which device.
The step sequencer is a module like any other: there can be several, each
with its own steps, rows, scale, rate and channel, sending notes to its own
synth, and one grid can be split between them in zones. Connections can
transform a moving value on its way, with a range and a curve, so a knob's
0 to 1 can be a tempo or a frequency without the module knowing about
knobs.

The core keeps the patch and checks every change against the modules'
definitions: kinds must match, an input that takes one connection takes
one, audio never goes round in a circle. A patch that cannot work is
refused when it is made, with the reason, instead of doing nothing later.

`trama play` builds its patch in code, with the connections a grid, the
gamepads and a synth need.

## A module has a core half and an engine half

The core half keeps the module's state and decides, on the core's thread,
where it may allocate and take its time. When the module keeps time or
makes sound, its engine half runs on the audio thread: a clock, a step
player or a node of the audio graph. The core half never touches the audio
thread; it sends engine commands named after its module. This keeps every
decision testable without a sound card, and everything on the beat inside
the engine, counted in samples.

## The engine patch carries the modules' state

When modules or connections change, the core describes the whole engine
patch again, with every pattern as it is, instead of telling the engine
what changed. The patch is complete on its own, so it does not matter
which commands reached the engine before it. A clock or step player in both
the old patch and the new one is the same one, by name: it keeps its place
in the bar and the note it holds, and notes that would be left hanging are
released where they were sent.

## One main clock, and nothing assumes it is the only one

Every project has a main clock, as a mixer has a main output, and modules
that follow a tempo follow it. The engine runs any number of clocks, each
step player names the one it follows, and a sequencer's `clock` input
takes another clock when one is connected. MIDI clock to a synth is a
connection, like notes.

## Names are fixed-size

Devices and modules are named with up to 32 lowercase letters, digits, `-`
and `_`. Engine commands, events and MIDI name the module or device they
are for, and they cross to and from the audio thread, which never
allocates; a name that fits in 32 bytes is copied like a number. Short
lowercase names are also easy to type in commands.

## Every gamepad is a device

Each gamepad is a device of its own, named `gamepad-1`, `gamepad-2` and so
on, so two players can have different jobs. `trama play` connects every
gamepad the same way when it appears.

## The audio graph is a patchbay

Audio goes through nodes connected by cables, the way gear is connected
with a patchbay, so anything that makes or changes sound plugs into the
same place. Each cable carries one channel: a microphone is one cable and
a stereo synth two, and nothing assumes a channel count. Cables that meet
at a jack are summed, as on a mixer. Loops are refused, because a graph
without loops has an order to run its nodes in.

The graph is a `GraphDescription` in `trama-protocol`, plain data that any
layer can write and that knows nothing about who wrote it. The core writes
it from the audio connections, and the engine prepares it into a graph it
can run.

## Patches are swapped, never built, on the audio thread

Building a patch allocates, and the audio thread must never allocate or
free. The controller prepares each patch, its graph included, on its own
thread and hands it over; the processor swaps it in and hands the old one
back to be freed. Patches travel in the same queue as the commands, so
everything reaches the processor in the order it was sent.

## Level changes glide over a fixed time

A gain that jumps clicks. Every change of level, mute included, glides
over 10 ms. A fixed time rather than one block keeps the sound the same
whatever the block size, which can be under a millisecond or near a
hundred.

## Input and output share one clock

Two sound cards each have a clock of their own, and over minutes they
drift apart: the input arrives a little faster or slower than the output
plays. Trama does not correct that. It plays an input only through an
output on the same clock, and the cushion between the two streams only
absorbs which callback runs first:

- on PipeWire, any input and output, because PipeWire runs every stream
  of one program in the same cycle and adapts the devices to it;
- elsewhere, the input of the output device itself. On macOS, an
  aggregate device made in Audio MIDI Setup joins several devices on one
  clock.

Trama refuses an input on another clock instead of letting it click every
few minutes. Correcting the drift means resampling, which touches only the
sound card adapter in `trama-io`: the engine receives blocks of input
either way.

## The cushion starts at one block and grows

Waiting for more input before playing it means fewer gaps and more delay.
The cushion starts at one block, the least that works when the output
callback runs before the input one, and grows by a block at each underrun,
up to four, so a system that needs more gets it and one that does not
keeps the shortest delay. Monitoring also feels the block size itself:
`--block-size` asks for smaller blocks.

## Inputs stay closed and monitoring starts muted

`trama play` opens an input only when `--audio-in` names one. Opening a
microphone shows that it is in use on Linux desktops and asks for
permission on macOS, which no one should meet just to run the sequencer.
When an input is open, its monitoring gains start muted unless `--monitor`
gives them a level, because a microphone next to speakers howls, and
nothing is connected until `--connect` says so.

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
