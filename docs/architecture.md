# Trama architecture

How Trama is put together: its layers, the messages between them, time,
threads and crates. The reasons behind the main choices are in
[decisions.md](decisions.md); how to build and test is in
[CONTRIBUTING.md](../CONTRIBUTING.md).

## The big picture

Trama connects instruments and controllers and plays them together, in time
with the sound card. It runs an 8-step sequencer: a grid controller edits
the pattern and shows the playhead, a gamepad adds pitch bend and
modulation, and a synth with a MIDI input plays the notes and follows
Trama's MIDI clock. Whatever reaches the sound card's inputs, a synth's
audio or a microphone, can play through Trama to its outputs.

Trama does three jobs, and each job is a layer:

| Layer      | Job                                                     | In one word |
|------------|---------------------------------------------------------|-------------|
| **IO**     | Talk to the outside world: devices and the sound card   | translate   |
| **Core**   | Know the state of the session and decide what happens   | decide      |
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
 │ CORE     session state, mapping      │         │
 └──────────────────────────────────────┘         │
        │ EngineCommand      ▲ EngineEvent        │
        ▼                    │                    │
 ┌────────────────────────────────────────────────────────────┐
 │ ENGINE   transport, MIDI clock, sequencer, audio graph     │
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
2. **Each piece of state has one owner.** The core owns the session, the
   engine owns what is sounding right now. Everyone else asks.
3. **The audio thread never waits.** Nothing on it allocates, locks or
   blocks.

## Messages

The message types live in `trama-protocol`.

| Message         | Meaning                                  | Example                                     |
|-----------------|------------------------------------------|---------------------------------------------|
| `Input`         | what happened on a device                | pad (3, 4) on `pads` pressed, velocity 0.79 |
| `Command`       | what the musician wants                  | toggle scale degree 3 on step 4             |
| `EngineCommand` | exactly what the engine must do          | step 4 plays F2, velocity 100               |
| `EngineEvent`   | what the engine reports back             | step 4 started at sample 1 512 000          |
| `Notification`  | what changed                             | light pad (3, 4) on `pads` blue             |
| `MidiOut`       | MIDI that must land on an exact sample   | note on F2 at sample 1 512 000              |
| `GraphDescription` | the audio patch the engine runs       | input 1 → gain → output 1 and output 2      |

`Input` is about devices ("this pad"). `Command` is about music ("this
step"). The **mapping** is the bridge between the two, and it works in both
directions: it turns `Input`s into `Command`s, and it decides how each
control should be lit to show the session. That is why some
`Notification`s are about the session (transport, steps, playhead) and
others are addressed to one device ("light pad (3, 4) on `pads` blue",
"send this pitch bend to `synth` now"). Adapters stay plain translators: the
Launchpad adapter knows how to light a pad, not what a lit pad means.

Adapters name controls by kind, never by model: a pad on a grid
(`Control::Pad`), the button beside a grid's row (`Control::RowButton`), a
grid's arrows (`up`, `down`, `left`, `right`), a gamepad's buttons and
sticks by their place on the Xbox layout (`south`, `left_stick_y`). A
mapping written for a kind of device works with every model of that kind.

`MidiOut` goes from the engine straight to the synth's output, without the
core, because notes and clock must land on the beat (see "Time").

### Walk-through: pressing a pad on the Launchpad

1. The Launchpad sends three bytes over USB: `90 2D 64` (note on, note 45,
   velocity 100).
2. The Launchpad adapter knows its layout: note 45 is the pad in row 3,
   column 4, counting from 0 at the bottom-left. It produces an `Input`:
   pad (3, 4) pressed with velocity 0.79.
3. The core's mapping says each column is a step and each row a scale
   degree, so the `Input` becomes the `Command` *toggle degree 3 on step 4*.
4. The core updates the pattern. In C2 minor, degree 3 is F2 (MIDI 41). It
   sends the engine *step 4 plays F2*, announces the new step, and asks for
   pad (3, 4) to be lit blue. The Launchpad adapter turns that into
   `90 2D 2D` (note 45, palette color 45).
5. When the transport reaches step 4, the engine, on the audio thread,
   writes a note on for F2 on that step's exact sample and reports that the
   step started.
6. The core moves the playhead: column 4 turns dim and pad (3, 4) green. The
   synth output sends the note on when its sample's moment comes.

### Why messages

- **Each layer is tested alone.** The core is fed messages and its answers
  are checked; the engine runs without a sound card; adapters are fed bytes.
- **A new model is only an adapter.** It names its controls by kind, so the
  mappings for that kind already work with it. A new kind of device is an
  adapter and a mapping; the session and the engine stay as they are.
- **No locks between layers.** Threads share messages, not data.

## State: who owns what

| Owner         | What it owns                                                                             |
|---------------|------------------------------------------------------------------------------------------|
| Core          | The session: pattern, tempo, transport, octave, root and scale, the last pitch bend and modulation sent, what each pad shows |
| Engine        | Its copy of the pattern, the note it is holding, the transport's position, the master clock, the audio graph it runs and the level of each gain |
| IO adapters   | Their connections; the synth output also keeps the MIDI waiting to be sent              |

The core is the single source of truth for the session. Anything that wants
to change it sends a `Command`.

## The audio graph

The engine runs an audio graph: nodes with jacks, and cables between them,
like a patchbay. A `GraphDescription` lists them:

- **nodes**: one channel of the sound card's input (`AudioInput`), one
  channel of its output (`AudioOutput`), a gain (`Gain`);
- **cables**: from an output jack of a node to an input jack of another.
  Every cable carries one channel, so stereo is two cables. An output jack
  can feed any number of cables, cables that reach the same input jack are
  summed, and cables never form a loop.

The description is plain data and knows nothing about who wrote it. The
engine's controller checks it and **prepares** it off the audio thread:
it works out the order to run the nodes in (every node after the ones it
listens to) and allocates a buffer for every output jack. Running a
prepared graph never allocates. Blocks longer than 1024 frames run in
pieces, so the buffers have a fixed size.

A channel the sound card does not have is silent, and a sample that is
not a number never reaches the card.

**Swap, don't build.** The controller sends a prepared graph through the
same queue as the commands, so the processor takes it in order: a command
sent after a graph reaches that graph. The processor swaps graphs at the
start of a block and hands the old one back through another queue, to be
freed on the controller's thread. It only swaps when that queue has room,
and the controller never has more graphs on their way than the queue
holds.

**Gains glide.** `SetGain` and `SetMuted` address a gain node by its id,
and the level glides to its new value over 10 ms, whatever the block
size, so changes do not click. A gain that is in both the old graph and
the new one keeps the level it was at and glides to its new one.

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

The cushion keeps input and output in step only if they share a clock
(see decisions.md). With PipeWire they always do: cpal puts both streams
in one group, and PipeWire runs them in the same cycle whatever the
devices. Elsewhere the input must be the output device itself; on macOS,
Audio MIDI Setup can join several devices into one aggregate device.
PipeWire also offers each output as an input that records what plays on
it; Trama leaves those out, so it never records itself.

The input opens with every channel the device offers, at the output's
sample rate and block size. `--block-size` asks the system for smaller
blocks, which shortens the delay between input and output.

## Time

**The sound card is the clock.** At 48 kHz with blocks of 1024 samples, it
asks for audio every 21 ms. The engine counts samples, and that count is
Trama's master clock. The transport turns the count and the tempo into MIDI
clock pulses (24 per quarter note) and sequencer steps.

- **Everything on the beat is decided inside the engine**, in samples: the
  MIDI clock, step starts, note ons and note offs.
- **Engine commands take effect at the start of the next block.**
- **MIDI to hardware**: the engine writes time-stamped MIDI into its own
  queue, and the synth output in IO sends it. This is the one path from the
  engine to IO that skips the core, because timing matters more than
  anything else there.
- **Live controls** (pitch bend, the modulation wheel) do not wait for the
  beat: the core addresses them to the synth, and the synth output sends
  them the moment they arrive.

### From samples to MIDI

A MIDI port only knows "send now", so the synth output turns "sample
96 000" into a moment on the computer's clock:

1. every block, the engine leaves an **anchor** in the MIDI queue: "the
   block starting at sample S was computed at instant T";
2. the synth output keeps the last 64 anchors and, for each, the offset
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
  │ core thread (the program's main thread)  │  session, mapping
  └──────────▲──────────────────┬────────────┘
             │  channels
  ┌──────────┴──────────────────▼────────────┐
  │ IO threads                               │  Launchpad input and
  │                                          │  lights, gamepads,
  │                                          │  synth MIDI output
  └──────────────────────────────────────────┘
```

The engine has two halves: the **controller**, on the core's thread, sends
`EngineCommand`s and prepared graphs and reads `EngineEvent`s; the
**processor**, on the audio thread, applies the commands, swaps the graphs
and computes each block. The graphs it stops running go back to the
controller through their own queue. Another queue carries `MidiOut` from
the processor to the synth output.

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
memory. A graph crosses as a pointer to memory the controller allocated,
and comes back the same way. `crates/trama-engine/tests/realtime.rs` runs the processor under
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
│   ├── trama-core/       the brain: session state, mapping, scales
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
- **`trama-core` knows nothing about devices or audio.** It is plain logic:
  every method takes a message and returns the messages to send.
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
| `midir`             | io             | MIDI ports (ALSA, CoreMIDI)                   |
| `gilrs`             | io             | gamepads (evdev, IOKit)                       |
| `crossbeam-channel` | io, program    | channels between normal threads               |
| `clap`              | program        | the command line                              |

## The program

`trama doctor` lists the audio outputs and inputs, with their channels, and
the MIDI ports and gamepads it finds. `trama play` runs the step sequencer
and monitors the sound card's inputs; everything it needs comes from its
command-line options.

`trama play` builds its audio graph from `--connect IN:OUT`: each input
channel named in a cable goes through a gain of its own, and its cables
meet at a monitoring level for each output channel, then reach that
output. Monitoring starts muted, because a microphone next to speakers
howls; `--monitor <dB>` turns it up with a command once the graph is in
place. Without `--connect`, nothing is connected.

Starting:

1. pick the sound card first, because the engine needs its sample rate,
   and the input, if `--audio-in` names one;
2. build the engine and split it into controller, processor and MIDI queue;
   hand the controller the monitoring graph and the monitoring level;
3. start the adapters: the synth output (with the MIDI queue), the
   Launchpad (switched to programmer mode) and the gamepads;
4. start the audio streams, which moves the processor to the audio thread;
5. run the core on the main thread: wait up to 1 ms for an input, handle
   it, then read whatever the engine reported and how the audio input is
   doing.

Stopping (Ctrl+C):

1. the core stops the transport, centers the pitch bend, sends "all notes
   off" and turns every light off; monitoring is muted;
2. the engine gets 100 ms to process the stop, release the last note and
   fade monitoring out;
3. the audio streams close;
4. the notification channels close, so each adapter finishes: the Launchpad
   goes back to its normal mode and the synth output sends what is left.
