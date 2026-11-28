# Trama architecture

How Trama is put together: its layers, the messages between them, time,
threads and crates. The reasons behind the main choices are in
[decisions.md](decisions.md); how to build and test is in
[CONTRIBUTING.md](../CONTRIBUTING.md).

## The big picture

Trama connects instruments and controllers and plays them together, in time
with the sound card. Inside, it is a **patch**: devices, modules that each
do one job (a clock, a step sequencer, a gain, a looper), and connections
between them. `trama play` builds one in code: an 8-step sequencer and a
looper that share a grid controller, gamepads that add pitch bend and
modulation, and a synth with a MIDI input that plays the notes and follows
Trama's MIDI clock. Whatever reaches the sound card's inputs, a synth's
audio or a microphone, can play through Trama to its outputs, and the
looper records it into loops.

Trama does three jobs, and each job is a layer:

| Layer      | Job                                                     | In one word |
|------------|---------------------------------------------------------|-------------|
| **IO**     | Talk to the outside world: devices and the sound card   | translate   |
| **Core**   | Know the patch and the modules' state; decide what happens | decide   |
| **Engine** | Keep time and place every event on an exact sample      | play        |

```
 grid controllers · gamepads · MIDI synths
        │  MIDI bytes, gamepad events            ▲  LED colors, MIDI
        ▼                                        │
 ┌────────────────────────────────────────────────────────────┐
 │ IO       adapters, one per kind of device                  │
 └────────────────────────────────────────────────────────────┘
        │ Input              ▲ Notification       ▲ MidiOut
        ▼                    │                    │ (notes and clock
 ┌──────────────────────────────────────┐         │  on exact samples)
 │ CORE     modules, connections        │         │
 └──────────────────────────────────────┘         │
        │ EngineCommand,     ▲ EngineEvent        │
        │ EnginePatch        │                    │
        ▼                    │                    │
 ┌────────────────────────────────────────────────────────────┐
 │ ENGINE   clocks, step players, audio graph with loopers    │
 └────────────────────────────────────────────────────────────┘
        ▲ input block                │ output block
        │                            ▼
 PipeWire or ALSA (Linux), CoreAudio (macOS) ↔ sound card
```

The sound card is Trama's clock. The piece of IO that opens it hands each
block of input to the engine and takes a block of output back; audio never
passes through the core.

Three rules hold everything together:

1. **Everything is a message.** Layers never reach into each other; they
   send the messages described below.
2. **Each piece of state has one owner.** The core owns the patch and
   the state of every module, the engine owns what is sounding right now.
   Everyone else asks.
3. **The audio thread never waits.** Nothing on it allocates, locks or
   blocks.

## Messages

The message types live in `trama-protocol`.

| Message         | Meaning                                  | Example                                     |
|-----------------|------------------------------------------|---------------------------------------------|
| `Input`         | what happened on a device                | pad (3, 4) on `pads` pressed, velocity 0.79 |
| `Command`       | what reaches an input of a module        | cell (3, 4) of the input `grid` of `sequencer` pressed |
| `EngineCommand` | exactly what the engine must do          | step 4 of `sequencer` plays F2, velocity 100 |
| `EngineEvent`   | what the engine reports back             | step 4 of `sequencer` started at sample 1 512 000 |
| `Notification`  | what changed                             | light pad (3, 4) on `pads` blue             |
| `MidiOut`       | MIDI that must land on an exact sample   | note on F2 for `synth` at sample 1 512 000  |
| `EnginePatch`   | everything the engine runs               | the main clock, the step player of `sequencer`, input 1 → gain → outputs 1 and 2 |

`Input` is about devices ("this pad"). `Command` is about modules ("this
cell of the sequencer's grid"). **Connections** are the bridge between the
two, and they work in both directions: they carry `Input`s to the inputs of
modules, and they carry the light outputs of modules back to controls.
That is why some `Notification`s are about a module (transport, steps,
playhead) and others are addressed to one device ("light pad (3, 4) on
`pads` blue", "send this pitch bend to `synth` now"). Adapters stay plain
translators: the Launchpad adapter knows how to light a pad, not what a lit
pad means.

Devices and modules have **names**: up to 32 lowercase letters, digits,
`-` and `_`, starting with a letter, such as `pads`, `gamepad-2` or
`sequencer`. A name is a fixed-size value, so messages that carry one
still cross to and from the audio thread without allocating.

Adapters name controls by kind, never by model: a pad on a grid
(`Control::Pad`), the button beside a grid's row (`Control::RowButton`), a
grid's arrows (`up`, `down`, `left`, `right`), a gamepad's buttons and
sticks by their place on the Xbox layout (`south`, `left_stick_y`). A
connection made for one model works with every model of that kind.

`MidiOut` goes from the engine straight to the synth's output, without the
core, because notes and clock must land on the beat (see "Time").

### Walk-through: pressing a pad on the Launchpad

1. The Launchpad sends three bytes over USB: `90 2D 64` (note on, note 45,
   velocity 100).
2. The Launchpad adapter knows its layout: note 45 is the pad in row 3,
   column 4, counting from 0 at the bottom-left. It produces an `Input`:
   pad (3, 4) pressed with velocity 0.79.
3. A connection goes from the Launchpad's 8 × 8 pads to the `grid` input
   of the module `sequencer`, so the `Input` reaches that input as a press
   on cell (3, 4). The sequencer's grid has a column per step and a row per
   scale degree: it puts degree 3 on step 4.
4. In C2 minor, degree 3 is F2 (MIDI 41). The sequencer sends the engine
   *step 4 of `sequencer` plays F2* and announces the new step. Another
   connection goes from the sequencer's `grid` light output back to the
   pads, so the core asks for pad (3, 4) to be lit blue. The Launchpad
   adapter turns that into `90 2D 2D` (note 45, palette color 45).
5. When the main clock reaches step 4, the sequencer's step player in the
   engine, on the audio thread, writes a note on for F2 on that step's
   exact sample, for every device its `notes` output is connected to, and
   reports that the step started.
6. The sequencer moves its playhead: column 4 turns dim and pad (3, 4)
   green. The synth output sends the note on when its sample's moment
   comes.

### Why messages

- **Each layer is tested alone.** The core is fed messages and its answers
  are checked; the engine runs without a sound card; adapters are fed bytes.
- **A new model is only an adapter.** It names its controls by kind, so
  connections made for that kind already work with it. A new kind of device
  is an adapter; the modules and the engine stay as they are.
- **No locks between layers.** Threads share messages, not data.

## State: who owns what

| Owner         | What it owns                                                                             |
|---------------|------------------------------------------------------------------------------------------|
| Core          | The patch: the devices it knows, the modules and the connections. Each module's state: a clock's tempo and transport; a sequencer's pattern, octave, playhead, last pitch bend and modulation; a gain's level; which of a looper's clips hold a recording, and what each of its tracks does and waits to do. What each control shows |
| Engine        | The patch it runs: each clock's position, each step player's copy of its pattern and the note it is holding, the master clock, the audio graph and the level of each gain, each looper's memory with its loops, and what its tracks play, record and wait for |
| IO adapters   | Their connections to the outside; the synth outputs also keep the MIDI waiting to be sent |

The core is the single source of truth for the patch and for every
module's state. Anything that wants to change a module sends a `Command`
to one of its inputs.

## Modules and connections

A **module** does one job. Its definition lists its inputs and outputs,
called ports, each with a name, a kind and whether it takes more than one
connection:

| Kind      | What travels                                             |
|-----------|----------------------------------------------------------|
| audio     | one channel of audio                                     |
| control   | presses, releases and moving values from controls        |
| notes     | notes and live controls such as pitch bend, as MIDI      |
| lights    | how controls are lit                                     |
| clock     | a tempo to follow; to a device, MIDI clock               |

A control input or a light output can be shaped like a grid, rows ×
columns, such as a sequencer's pattern.

A module has two halves. Its **core half** keeps its state and decides:
it receives what reaches its inputs, says how its light outputs light a
control when asked, and sends what it produces to its outputs by name. It
never knows what it is connected to. When the module keeps time or makes
sound it also has an **engine half**, which runs on the audio thread: a
clock, a step player, or a node of the audio graph.

Trama's modules:

| Module         | Inputs                                                | Outputs                         | Engine half |
|----------------|-------------------------------------------------------|---------------------------------|-------------|
| clock          | `play`, `tempo_up`, `tempo_down`, `tempo` (BPM)       | `clock`; lights of `play`, `tempo_up`, `tempo_down` | a clock |
| step sequencer | `grid` (rows × steps), `clear`, `octave_up`, `octave_down`, `pitch_bend`, `modulation`, `clock` | `notes`; lights of `grid`, `clear`, `octave_up`, `octave_down` | a step player |
| gain           | `in` (audio), `level` (dB), `mute`                    | `out` (audio); light of `mute`  | a gain node |
| looper         | `clips` (scenes × tracks), `scenes` (a column), `stop` (a row), `delete`, `clock`; audio `in_1`, `in_2`… per track and channel | audio `out_1`, `out_2`… per track and channel; lights of `clips`, `scenes`, `stop`, `delete` | a looper node |

A sequencer has settings: steps (1 to 64), rows, root, scale, rate and MIDI
channel. Any number of sequencers can play at once, each on its own
channel and to its own devices.

A looper has settings too: tracks and scenes (1 to 16 each), the channels
of every track (1, or 2 for stereo: `in_1_left`, `in_1_right`…), whether
its loops follow a clock or are free, the seconds of recording its clips
share, and what a press on the clip a track plays does: stop it, or play
it again from its start. A free looper has no `clock` input. A press on a
clip records into it when it is empty, ends the recording when it records,
plays it when it holds a recording; one with `delete` held empties it. A
press on what a track waits to do cancels it.

Every project has one **main clock**, the module `clock`, and it cannot be
removed. A module that follows a tempo follows the clock connected to its
`clock` input, or the main clock when nothing is. MIDI clock to a synth is
a connection from a clock's `clock` output to that synth.

A **connection** goes from what produces to what receives:

| From                                       | To                                              |
|--------------------------------------------|-------------------------------------------------|
| a control of a device, a zone of a grid's pads, or the buttons beside some of its rows | a control input of a module |
| a light output of a module                 | a control, a zone of pads, or buttons beside rows |
| a notes or clock output                    | a device's MIDI input                           |
| a channel of the sound card's input, or an audio output | an audio input, or a channel of the sound card's output |
| a clock output                             | a clock input                                   |

- A **zone** is a rectangle of a grid's pads. It meets a grid-shaped input
  or output with its bottom-left pad on cell (0, 0), so one grid can be
  split between several modules: the bottom four rows for one sequencer,
  the top four for another. The buttons beside consecutive rows meet an
  input or output shaped like a column the same way, the lowest on cell
  (0, 0), as a looper's scene buttons do.
- A connection to a control input can **transform** moving values: from
  one range to another, along a straight line or an exponential curve. A
  knob's 0.0 to 1.0 becomes 60 to 180 BPM; a stick's -1.0 to 1.0, turned
  upside down, becomes 1.0 to -1.0.
- A control may feed any number of inputs, but **only one connection
  lights it**, so a button shows exactly one thing.
- An input that takes one connection refuses a second, and audio that
  would go round in a circle is refused.

The core checks every connection when it is made and refuses those that
cannot work, saying why.

### The engine patch

When modules or connections change, the core describes everything the
engine must run as an `EnginePatch`: the audio graph, the clocks with the
devices that get their MIDI clock, and the step players with their
pattern, channel and devices. Each clock and step player is named after
its module. The patch carries the modules' state, such as the patterns,
so it is complete on its own.

The engine prepares the patch off the audio thread and swaps it in, the
way "The audio graph" describes, so modules and connections change while
Trama plays:

- a clock in both patches keeps its place in the bar;
- a step player in both keeps holding its note, if it still sends to the
  same devices on the same channel; otherwise the note is released where
  it was sent;
- a device that stops getting a playing clock gets a stop;
- a looper in both keeps its memory and its loops (see "Loops").

## The audio graph

The engine runs an audio graph, part of the engine patch: nodes with
jacks, and cables between them, like a patchbay. A `GraphDescription` lists
them:

- **nodes**: one channel of the sound card's input (`AudioInput`), one
  channel of its output (`AudioOutput`), a gain (`Gain`), a looper
  (`Looper`), whose jacks are its tracks' channels;
- **cables**: from an output jack of a node to an input jack of another.
  Every cable carries one channel, so stereo is two cables. An output jack
  can feed any number of cables, cables that reach the same input jack are
  summed, and cables never form a loop.

The core writes it from the audio connections: each gain module is a gain
node, each looper a looper node, and each channel of the sound card in use
an input or output node.
The description is plain data and knows nothing about who wrote it. The
engine's controller checks it and **prepares** it off the audio thread:
it works out the order to run the nodes in (every node after the ones it
listens to) and allocates a buffer for every output jack. Running a
prepared graph never allocates. Blocks longer than 1024 frames run in
pieces, so the buffers have a fixed size.

A channel the sound card does not have is silent, and a sample that is
not a number never reaches the card.

**Swap, don't build.** The controller sends a prepared patch, graph
included, through the same queue as the commands, so the processor takes
it in order: a command sent after a patch reaches that patch. The
processor swaps patches at the start of a block and hands the old one back
through another queue, to be freed on the controller's thread. It only
swaps when that queue has room, and the controller never has more patches
on their way than the queue holds.

**Gains glide.** `SetGain` and `SetMuted` address a gain node by its id,
and the level glides to its new value over 10 ms, whatever the block
size, so changes do not click. A gain that is in both the old graph and
the new one keeps the level it was at and glides to its new one.

## Loops

A looper is a node of the audio graph with a track per column of its clip
grid. Each track records what reaches its input jacks into one of its
clips, and plays one clip at a time to its output jacks. The core says
what a track does next (`QueueTrack`: record a clip, play one, stop), and
the engine picks the sample it happens on and reports it
(`EngineEvent::Track`), with the clips that gain or lose a recording
(`EngineEvent::Clip`).

- **On a clock**, a looper acts on the clock's bars: what a track is asked
  to do waits for the first sample of the next bar, and a recording lasts
  whole bars. A clip plays at the speed that fits its bars into the
  clock's tempo, like tape, so a faster tempo is also higher. The engine
  works out the place in the clip from the clock's place in the bar, so a
  loop never drifts from the clock. At another speed than it was recorded
  at, a clip is interpolated between its samples, along a cubic (Hermite)
  curve. When the clock stops, the looper's tracks stop; a recording keeps
  its whole bars.
- **Free**, a looper acts at the start of the next block and plays its
  clips at the speed they were recorded at, so loops of different lengths
  drift against each other.
- A clip fades in and out over 3 ms when it starts and stops, and its last
  10 ms are crossfaded with the 10 ms that came before its first sample, so
  its end leads into its start without a click.

### Walk-through: recording a loop

1. A press on pad (5, 2) reaches the looper as a press on cell (1, 2) of
   `clips`: scene 1 of track 2. The clip is empty, so the looper asks the
   engine to record it, and lights the pad flashing red.
2. On the first sample of the next bar, the engine starts the recording and
   reports it. The pad turns solid red.
3. Another press asks the engine to play the clip. On the first sample of
   the bar after that, the engine ends the recording, which lasts whole
   bars, and plays it from its start. The pad turns green.

### Memory

Recording must never allocate, so a looper's memory is allocated when it
joins the patch, off the audio thread: chunks of 4096 frames for the
seconds its settings give, written to once so the system has provided
every page before the audio thread needs it. A clip takes chunks as it
records and gives them back when it is emptied. A recording that finds no
chunk left keeps what fits, in whole bars on a clock, and plays it.

The engine's controller remembers the loopers of the last patch it sent.
In the next patch, a looper with the same node id and the same shape gets
no memory of its own, and the swap moves the running looper's memory,
loops and all, into it. The loops are the one piece of a module's state
that only the engine has: they are audio, and they never cross to the
core.

### Latency

A sound played along with what the speakers play reaches the engine
later: the time the output takes to be heard, the time the input takes to
arrive, and the cushion between them. The sound card adapter adds up what
the system reports for each and what waits in the cushion, smoothed over a
second or so, and `trama play` hands that round trip to the engine
(`SetLatency`), plus `--latency-trim`. A looper starts and ends every
recording that much later than the bar, so the loop holds what was played
on the beat.

## Audio input

cpal opens a sound card's input and output as two streams, each with a
callback on an audio thread of its own. The input callback writes into a
lock-free ring buffer, and the output callback reads from it and hands the
engine both blocks:

```
 input stream ──▶ ring buffer (the cushion) ──▶ output stream ──▶ engine
```

Either callback may run first in a cycle, so the output waits for a
**cushion** of one block of input before it plays any, and plays silence
until then. When the input runs out (an **underrun**) the missing frames
are silent, the cushion grows by a block, up to four, and the output
waits for it to fill again. Input that finds the ring full, or that piles
up beyond the cushion while the output was not reading, is dropped (an
**overrun**). `trama play` reports both, at most once a second.

With PipeWire, input and output always share a clock: cpal puts both
streams in one group, and PipeWire runs them in the same cycle whatever
the devices. Elsewhere only the input of the output device itself does.
PipeWire also offers each output as an input that records what plays on
it; Trama leaves those out, so it never records itself.

The input opens with every channel the device offers and the output's
block size. `--block-size` asks the system for smaller blocks, which
shortens the delay between input and output. On the output's clock it
opens at the output's sample rate, and the cushion is all there is.

### An input on a clock of its own

Two clocks drift apart, typically by around 50 parts per million: the
input arrives a few samples a second faster or slower than the output
plays it, and the cushion would fill up or run dry every few minutes. So
an input on a clock of its own goes through a resampler on its way to the
cushion (see decisions.md):

```
 input stream ──▶ resampler ──▶ cushion ──▶ output stream ──▶ engine
                      ▲                          │
                      └──────── correction ──────┘
```

- The input opens at the sample rate its device runs at, and the
  resampler (rubato's sinc resampler) converts it to the output's: 44.1
  kHz into 48 kHz, or 48 kHz into 48 kHz on another clock. The engine
  receives blocks of input at the output's rate either way.
- The output callback measures the cushion's fill and steers the ratio,
  stretching or squeezing the input by the drift, up to 0.5 %. Input
  blocks arrive at moments that slide slowly past the reads, so the fill
  it measures counts what the input has captured since its last block, as
  if input arrived sample by sample. The correction travels to the input
  callback through an atomic, like the counters.
- A slow control loop keeps that fill an input block and a quarter above
  the cushion, so the ring does not run dry just before a block arrives,
  when it holds least. It settles in about fifteen seconds and smooths out
  the jitter of callbacks, so the pitch never wobbles audibly.

The resampler and its buffers are made when the stream starts; on the
audio thread it never allocates, and its work grows only with the block.
`crates/trama-io/src/audio/clocks_apart.rs` runs both streams offline on
clocks 100 ppm apart, and from 44.1 kHz to 48 kHz, for minutes: no
underruns or overruns once settled, a sine stays a clean sine, and the
resampler never allocates.

## Time

**The sound card is the clock.** At 48 kHz with blocks of 1024 samples, it
asks for audio every 21 ms. The engine counts samples, and that count is
Trama's master clock. Each clock of the patch turns the count and its
tempo into MIDI clock pulses (24 per quarter note) while it plays, and each
step player turns the pulses of the clock it follows into steps: 6 pulses
a step for sixteenth notes, 12 for eighth notes. A clock's bars have four
beats, 96 pulses, counted from the pulse it starts on; loopers act on
them. A new tempo takes effect at the start of the next block, the pulse
under way included, so a clock keeps its place in the bar.

- **Everything on the beat is decided inside the engine**, in samples: the
  MIDI clock, step starts, note ons and note offs, and when loops start,
  stop and record.
- **Engine commands take effect at the start of the next block.**
- **MIDI to hardware**: the engine writes time-stamped MIDI into its own
  queue, each message naming the device it is for, and the synth outputs
  in IO send it. This is the one path from the engine to IO that skips the
  core, because timing matters more than anything else there.
- **Live controls** (pitch bend, the modulation wheel) do not wait for the
  beat: the core addresses them to the devices the sequencer's `notes`
  output is connected to, and the synth outputs send them the moment they
  arrive.

### From samples to MIDI

A MIDI port only knows "send now", so the synth outputs turn "sample
96 000" into a moment on the computer's clock:

1. every block, the engine leaves an **anchor** in the MIDI queue: "the
   block starting at sample S was computed at instant T";
2. the synth outputs keep the last 64 anchors and, for each, the offset
   between T and S. Audio callbacks can wake up late but never early, and a
   stream starts with a burst of blocks to fill the sound card's buffer, so
   it uses the **smallest** recent offset: the callback that was most on
   time;
3. each message is sent at its sample's time plus a fixed 10 ms, so it is
   never late because the MIDI thread itself woke up a little late.

Measured against a virtual synth on Linux with PipeWire, MIDI clock pulses
come out within a fraction of a millisecond of where they belong.

## Threads

```
  ┌──────────────────────────────────────────┐
  │ audio threads (real-time)                │  engine processor on the
  │                                          │  output stream, input stream
  └──────────▲──────────────────┬────────────┘
             │  lock-free queues: fixed size, never block
  ┌──────────┴──────────────────▼────────────┐
  │ core thread (the program's main thread)  │  patch, modules  
  └──────────▲──────────────────┬────────────┘
             │  channels
  ┌──────────┴──────────────────▼────────────┐
  │ IO threads                               │  Launchpad input and
  │                                          │  lights, gamepads,
  │                                          │  synth MIDI outputs
  └──────────────────────────────────────────┘
```

The engine has two halves: the **controller**, on the core's thread, sends
`EngineCommand`s and prepared patches and reads `EngineEvent`s; the
**processor**, on the audio thread, applies the commands, swaps the
patches and computes each block. The patches it stops running go back to
the controller through their own queue. Another queue carries `MidiOut`
from the processor to the synth outputs.

### The audio thread never waits

The audio thread is like a drummer who can never miss a beat. If it is late
even once, the sound clicks and the clock stumbles. So the processor
**never**:

- allocates or frees memory;
- takes a lock (waits for another thread);
- touches files or the network;
- prints, logs, or calls anything that might do one of the above.

Everything that crosses to and from the audio thread (commands, events,
MIDI, anchors) is small, fixed-size and copied, so the queues never need
memory; that is why names are fixed-size. A patch crosses as a pointer to
memory the controller allocated, and comes back the same way. `crates/trama-engine/tests/realtime.rs` runs the processor under
`assert_no_alloc`, which aborts the test if the audio path allocates.

## Crates

Trama is a Cargo **workspace**: one repository with several packages
(*crates*) built and tested together.

```
trama/
├── Cargo.toml            the workspace: list of crates and shared settings
├── crates/
│   ├── trama-protocol/   the messages every layer speaks
│   ├── trama-engine/     the real-time engine (no devices, no sound card)
│   ├── trama-core/       the brain: the patch, modules, connections, scales
│   ├── trama-io/         adapters: sound card in and out, Launchpad X, synths, gamepads
│   └── trama/            the program: wires everything together
├── docs/                 this document and the decision log
└── scripts/              repository checks
```

Who may depend on whom:

```
                trama  (the program)
         ┌────────┼────────────┐
         ▼        ▼            ▼
     trama-io  trama-core  trama-engine
         │        │            │
         ▼        ▼            ▼
              trama-protocol
```

- **Arrows only point down.** Nothing depends on the program, and
  `trama-protocol` depends on no other Trama crate.
- **`trama-core` knows nothing about devices or audio.** It knows devices
  only by name, and is plain logic: every method takes a message or a
  change and returns the messages to send.
- **`trama-engine` knows nothing about devices or the sound card.** It is
  "audio in, audio out", so tests run it offline.
- **`trama-io` knows nothing about the core or the engine.** It translates;
  the sound card adapter takes a render function, not the engine.
- **Only `trama` knows everyone**, and its job is to connect them.
- **No crate uses `unsafe`**, enforced by the workspace lints.

Splitting into crates makes the compiler enforce these boundaries: the
engine cannot call the MIDI library by accident because it does not depend
on it.

The main outside libraries:

| Library             | Used by        | For                                           |
|---------------------|----------------|-----------------------------------------------|
| `rtrb`              | engine, io     | lock-free queues to and from the audio threads |
| `cpal`              | io             | the sound card (PipeWire, ALSA, CoreAudio)    |
| `rubato`            | io             | resampling an input on a clock of its own     |
| `midir`             | io             | MIDI ports (ALSA, CoreMIDI)                   |
| `gilrs`             | io             | gamepads (evdev, IOKit)                       |
| `crossbeam-channel` | io, program    | channels between normal threads               |
| `clap`              | program        | the command line                              |

## The program

`trama doctor` lists the audio outputs and inputs, with their channels, and
the MIDI ports and gamepads it finds. `trama play` runs the step sequencer
and the looper, and monitors the sound card's inputs; everything it needs
comes from its command-line options.

`trama play` builds its patch in code:

- the module `sequencer`, with the root, scale, rate and channel of the
  command line, on the main clock at `--bpm`, with a row for each row of
  the grid the looper leaves it;
- the loopers, with a scene for each of the grid's top `--looper-rows`
  rows (4 unless it says otherwise): `looper` on the main clock, and
  `free-looper` for the right `--free-columns` columns. Each track records
  the input channels `--loop` names, mono or stereo, and plays to its
  output channels;
- the device `synth` gets the sequencer's notes and the main clock;
- the device `pads`, a grid: its top rows to the loopers' clips and back,
  and the buttons beside them to the scenes of both loopers; the rows
  below to the sequencer's grid and back. The buttons beside the
  sequencer's rows go, from the bottom, to the clock's `play`, the
  loopers' `delete` and the sequencer's `clear`, as many as there are
  rows. Up and down go to the octave, left and right to the tempo, each lit
  by what it does;
- every gamepad, a device of its own (`gamepad-1`, `gamepad-2`, …),
  connected when it appears: `south` to `play`, the D-pad like the grid's
  arrows, the left stick to `pitch_bend`, the right trigger to
  `modulation`;
- for `--connect IN:OUT`, a gain called `monitor-OUT` for each output
  channel, which every input channel cabled to it goes through. The gains
  start muted, because a microphone next to speakers howls, unless
  `--monitor <dB>` gives their level. Without `--connect`, nothing is
  connected.

Starting:

1. pick the sound card first, because the engine needs its sample rate,
   and the input, if `--audio-in` names one;
2. build the engine and split it into controller, processor and MIDI queue;
   build the core with the sequencer and monitoring;
3. start the adapters: the synth outputs (with the MIDI queue), the
   Launchpad (switched to programmer mode) and the gamepads, connecting
   each device that is there; hand the controller the engine patch;
4. start the audio streams, which moves the processor to the audio thread;
5. run the core on the main thread: wait up to 1 ms for an input, handle
   it, then read whatever the engine reported and how the audio input is
   doing, and hand the engine the round trip when it moves by a
   millisecond or more.

Stopping (Ctrl+C):

1. the core stops the main clock, and with it the looper on it, stops the
   free looper's tracks, centers the pitch bend, sends "all notes off",
   mutes the gains and turns every light off;
2. the engine gets 100 ms to process the stop, release the last note and
   fade monitoring out;
3. the audio streams close;
4. the notification channels close, so each adapter finishes: the Launchpad
   goes back to its normal mode and the synth outputs send what is left.
