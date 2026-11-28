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
gamepads and a synth need. It shares one grid between the sequencer and
the loopers, the line between them set by `--looper-rows`.

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

A looper's recordings are the exception: they are audio, minutes of it,
and only the engine holds them. A looper in both patches, by node id, keeps
its memory and its loops.

## One main clock, and nothing assumes it is the only one

Every project has a main clock, as a mixer has a main output, and modules
that follow a tempo follow it. The engine runs any number of clocks, each
step player names the one it follows, and a sequencer's `clock` input
takes another clock when one is connected. MIDI clock to a synth is a
connection, like notes.

## Bars have four beats

A clock counts bars of four beats from the pulse it starts on, and loopers
act on them. It is the bar most music people loop is in. The number of
beats is part of the clock's description in the engine patch, so it can
differ from one clock to another.

## Loops follow the tempo like tape

A loop on a clock lasts whole bars, and when the tempo changes it plays
faster or slower to keep filling them, which moves its pitch too, the way
tape does. Keeping the pitch while changing the speed (time-stretching)
sounds better but costs far more and makes sounds of its own. The place in
a loop comes from the clock's place in the bar, sample by sample, so a loop
never drifts from the clock however long it plays; at the speed it was
recorded at, it plays its samples as they are.

## A looper is on a clock, or free

Each looper keeps time one way: its loops follow a clock, start and stop on
its bars and last whole bars, or they are free, start and stop at once and
last as long as they were recorded. Both kinds on one grid are two
loopers, each with a zone of the grid, and the scene buttons can go to
both, since a control may feed any number of inputs. Every module keeps
its own settings this way.

## Each track of a looper has its own audio

Every track has an input and an output jack per channel, mono or stereo,
so one track can loop a synth and another a microphone, and each loop can
go through a gain of its own on the way out. Cables that meet at a track's
input are recorded together, as on a mixer.

## A press on a playing clip is a setting

By default a press on the clip a track plays stops it on the next bar, so a
grid needs no row of stop buttons. A looper can instead play the clip again
from its start, as some clip launchers do, and then stops tracks with its
`stop` input. A press on what a track waits to do cancels it.

## Loops live in memory taken up front

Recording on the audio thread must not allocate, nor wait for the system
to provide a page the first time it is written. A looper takes the memory
for all its recording time when it joins the patch, in chunks, and writes
to every page then. Clips take chunks as they record and give them back
when they are emptied, so one long loop or many short ones fit in the
same memory. A chunk given back goes to the end of the queue of free
chunks, so a clip emptied while it plays keeps its sound for the few
milliseconds it takes to fade out.
When the memory is full, a recording keeps what fits, in whole bars on a
clock.

## Recordings start one round trip late

What reaches the input was played along with what the speakers played a
moment before: the output's latency, the input's, and the cushion between
them. A loop recorded from the bar as it arrives would sound late by all of
that. The sound card adapter adds up what the system reports for each part
and what waits in the cushion, and a looper starts and ends every recording
that much later than the bar. `trama play` takes milliseconds off or adds
them with `--latency-trim`, for latency the system does not report, such as
an interface's converters.

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

## An input on another clock is resampled

Two sound cards each have a clock of their own, and over minutes they
drift apart: the input arrives a little faster or slower than the output
plays, and the cushion between the two streams fills up or runs dry. Most
people's gear is like that: a laptop's built-in microphone and its
headphones are two devices. So Trama plays any input through any output,
and resamples an input on another clock than the output's:

- Pairs on one clock skip the resampler, and with it its delay and its
  CPU: on PipeWire every input and output, because PipeWire runs every
  stream of one program in the same cycle and adapts the devices to it;
  elsewhere, the input of the output device itself.
- The resampler is rubato's sinc resampler. It lets the ratio change
  while it runs, keeps aliasing inaudible and never allocates once made,
  and its license, MIT, is GPL-3.0 compatible. One of our own would sound
  worse for the same work.
- The cushion's fill measures the drift, and a slow loop steers the ratio
  from it: it follows the drift and lets the jitter of callbacks pass, so
  the pitch never moves by an audible amount.
- The input keeps the sample rate its device runs at. Asking for the
  output's would change the device for every program using it, and the
  resampler handles any rate.

Resampling touches only the sound card adapter in `trama-io`: the engine
receives blocks of input at the output's rate either way.

## The cushion starts at one block and grows

Waiting for more input before playing it means fewer gaps and more delay.
The cushion starts at one block, the least that works when the output
callback runs before the input one, and grows by a block at each underrun,
up to four, so a system that needs more gets it and one that does not
keeps the shortest delay. A resampled input is kept an input block and a
quarter above the cushion, because its blocks arrive at moments that
slide past the reads. Monitoring also feels the block size itself:
`--block-size` asks for smaller blocks.

## Inputs stay closed and monitoring starts muted

`trama play` opens an input only when `--audio-in` names one. Opening a
microphone shows that it is in use on Linux desktops and asks for
permission on macOS, which no one should meet just to run the sequencer.
When an input is open, its monitoring gains start muted unless `--monitor`
gives them a level, because a microphone next to speakers howls, and
nothing is connected until `--connect` says so.

## A server owns the open project

Trama runs as a server with one project open, and every other way to use
it, a command in another terminal or a web page, is a client of one API.
One sound card and one set of controllers can serve one project, and the
state has one owner, so several clients at once see the same project and
the same changes, and nothing needs a screen. The core stays plain logic
on a thread of its own: requests reach it as messages, one at a time, as
inputs do.

The scratch project is open when the server starts. It lives in `scratch/`
in the user data folder, where the XDG rules on Linux and Application
Support on macOS put a program's data, laid out like a project folder.
It has no devices, so the server opens only the system's default audio
output, the clock every project needs, and no input, as "Inputs stay
closed" says. Without a sound card it runs anyway, and says that nothing
plays, so it also runs in tests and on machines without audio.

## The API is HTTP and a WebSocket on localhost

Requests are HTTP with JSON, which browsers, `curl` and any language
speak. Changes come over a WebSocket, which browsers have and which pushes
them as they happen instead of being asked again and again. The port is
8726, "TRAM" on a phone's keypad; `--port` picks another when it is taken.

The API runs on axum, the most maintained of Rust's HTTP servers, with
WebSockets built in, on a tokio runtime of one thread. Async code stays
inside the API's thread: the rest of Trama keeps its threads and
channels. Commands use ureq, a blocking HTTP client without TLS, since
they only talk to localhost. All three are MIT or Apache-2.0.

## Clients get a snapshot, then every change

A client that connects gets the whole status first and every change after
it. The project's thread puts both in the client's queue, in the order
they happened, so a client never misses a change between the two or sees
one twice. A client that falls 4096 changes behind is dropped rather than
waited for: a phone on a slow network must never slow the core, and it
reconnects to a fresh snapshot. Lights and MIDI are for devices, and there
are many of them, so clients get what modules show instead: transport,
steps, playheads.

## Pages of other sites cannot drive Trama

Listening on localhost keeps other computers out, but not the web pages
open in the user's browser, which can send requests to localhost. One
could start the transport, or turn up a gain. So the server answers only requests whose `Host` is
localhost with its port, which defeats a site that points its own name at
this computer, and refuses any `Origin` other than its own pages. Controls
must be JSON, which a page of another site cannot post without the
browser asking first, and the server never says yes.

## One server at a time, found through a lock

Two servers would fight over the sound card, the controllers and the
scratch project. The server holds a lock file in the user data folder,
locked by the system, so a crash releases it and a stale file never
blocks the next start. The file also holds the port, so commands find
the server on any port without being told, and a second server or `trama
play` can say where the first one is.

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
