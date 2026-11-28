# Trama

Trama connects synths, MIDI controllers and gamepads and plays them together,
in time with the sound card, without a DAW. It is free software written in
Rust, and runs on Linux and macOS.

It runs an 8-step sequencer and a looper that share a grid controller. The
grid edits the pattern and shows the playhead, gamepads add pitch bend and
modulation, and a synth with a MIDI input plays the notes and follows
Trama's MIDI clock. What reaches the sound card's inputs, a synth's audio
or a microphone, can play through Trama to its outputs, and the looper
records it into loops that follow the tempo, or ignore it.

Inside, the sequencer and the looper are modules, connected to the devices
the way a patchbay connects gear; [the architecture](docs/architecture.md)
explains modules and connections.

## Supported devices

| Kind             | What works                                                            |
|------------------|-----------------------------------------------------------------------|
| Sound cards      | any output and input the system offers (PipeWire or ALSA on Linux, CoreAudio on macOS), with every channel they have; the input must share the output's clock (see below) |
| Synths           | anything with a MIDI input: hardware, or a program on a virtual MIDI port |
| Grid controllers | Novation Launchpad X                                                  |
| Gamepads         | those gilrs recognises (evdev on Linux, IOKit on macOS)               |

## Build

Install Rust from [rustup.rs](https://rustup.rs) and the system libraries:

- Debian and Ubuntu: `sudo apt install pkg-config libasound2-dev libpipewire-0.3-dev libclang-dev libudev-dev`
- Fedora: `sudo dnf install alsa-lib-devel pipewire-devel clang-devel systemd-devel`
- macOS: `xcode-select --install`

## Run

See what Trama can find (audio outputs and inputs with their channels, MIDI
ports, gamepads):

```sh
cargo run -- doctor
```

Play the step sequencer on a synth, naming its MIDI port (part of the name
`trama doctor` shows):

```sh
cargo run --release -- play --synth "My Synth"
```

Without `--synth`, Trama creates a virtual MIDI port named "trama synth" for
a synth or another program to connect to. It uses a grid controller and
gamepads if they are connected.

### Playing the sound card's inputs

Trama opens an input only when `--audio-in` names one, and `--connect
IN:OUT` patches input channel IN to output channel OUT, counting from 1.
Monitoring starts muted, because a microphone next to speakers howls;
`--monitor` turns it up, in decibels. A microphone on input 1, on both
speakers, at about half its level:

```sh
cargo run --release -- play --audio-in default --connect 1:1 --connect 1:2 --monitor -6
```

A stereo synth on inputs 1 and 2 would be `--connect 1:1 --connect 2:2`.
Each cable carries one channel; cables that reach the same output are
mixed. Trama opens every channel an input has, but on PipeWire `default`
always has two: to reach the others, name the device as `trama doctor`
lists it.

Any input plays through any output. Each device keeps time with a clock
of its own, and two clocks drift apart by a few samples a second, so an
input on another clock than the output is resampled to it:

- **Linux with PipeWire**: every input and output run on one clock, and
  nothing is resampled.
- **Linux without PipeWire, and macOS**: the input of the output device
  itself shares its clock. Any other input, such as the built-in
  microphone with headphones, is resampled, and `trama play` says so. It
  keeps the sample rate its device runs at, so an input at 44.1 kHz plays
  through an output at 48 kHz.

macOS also asks whether the terminal may use the microphone; until it may,
the input is silent, and Trama says so.

Between input and output there is a delay of a few blocks, and about one
more when the input is resampled. At 48 kHz a block of 1024 frames lasts
21 ms; `--block-size 128` asks for blocks of under 3 ms, at the cost of
more CPU. Trama reports underruns and overruns (input that came too late
or too early) as they happen.

### Looping

The top four rows of the grid are the looper's clips: a column per track,
a row per scene. The bottom four are the sequencer's, which then has four
scale degrees. `--looper-rows` moves the line: `--looper-rows 2` leaves six
rows to the sequencer, `--looper-rows 0` gives it the whole grid.

`--loop IN:OUT` says what the looper's tracks record and where they play,
like `--connect`: once for mono tracks, twice for stereo. A synth on inputs
1 and 2, looped on both speakers:

```sh
cargo run --release -- play --audio-in "My Interface" --loop 1:1 --loop 2:2
```

Every track records those inputs. Loops follow the main clock: a press on
an empty clip records from the next bar, another press ends the recording
at the end of a bar and plays it, and a press on a clip that plays stops
it on the next bar. A loop lasts whole bars and follows the tempo like
tape: faster is also higher. Loops are free instead in the right
`--free-columns` columns: they start and stop at once, last as long as
they were recorded, and ignore the tempo, so loops of different lengths
drift against each other.

The loops live in memory, five minutes of recording for the clips on the
clock and five more for the free ones, about 115 MB each in stereo. When
it is full, a recording keeps what fits, in whole bars on the clock.

What reaches the input arrives a little late: the time to be heard, then
to come back through the input. Trama measures that round trip and records
every loop that much later, so it lines up with what was heard, and says
how long it is when it starts. If loops still sound early or late,
`--latency-trim` adds or takes away milliseconds.

Useful options (`cargo run -- play --help` lists them all):

| Option             | Default    | Meaning                                                   |
|--------------------|------------|-----------------------------------------------------------|
| `--synth <name>`   | virtual    | synth MIDI port; `virtual` makes a port to connect to     |
| `--pads <name>`    | a Launchpad X, if connected | grid controller MIDI port; `none` leaves it out |
| `--channel <n>`    | 1          | MIDI channel the synth listens on                         |
| `--root <note>`    | C2         | note of the bottom row                                    |
| `--scale <name>`   | minor      | major, minor, dorian, phrygian, pentatonic-minor, chromatic |
| `--bpm <n>`        | 120        | tempo                                                     |
| `--rate <n>`       | 16         | step length: 4, 8, 16 or 32                               |
| `--audio <name>`   | default    | sound card output; it is also the clock               |
| `--audio-in <name>` | none      | sound card input; `default` for the system's          |
| `--connect <in:out>` | nothing  | patch an input channel to an output channel; repeat it |
| `--monitor <dB>`   | muted      | monitoring volume: 0 as loud as it comes in, +12 at most |
| `--looper-rows <n>` | 4         | rows of the grid, from the top, that are looper clips: 0 to 7 |
| `--free-columns <n>` | 0        | looper columns, from the right, whose loops are free  |
| `--loop <in:out>`  | nothing    | what every looper track records and where it plays; twice for stereo |
| `--latency-trim <ms>` | 0       | milliseconds added to the measured round trip         |
| `--block-size <n>` | the system's | frames per audio block; smaller is less delay       |

Controls:

- **Grid, looper rows**: a press on an empty clip records, on the clip
  recording plays it, on the clip playing stops it; another clip of a
  track takes its place on the next bar. A clip waiting for the bar flashes
  the color it is about to have: red to record, green to play, yellow to
  stop. The buttons beside these rows play a whole scene, and stop the
  tracks that have nothing in it.
- **Grid, sequencer rows**: each column is a step, each row a scale degree
  (root at the bottom). The buttons beside them, from the bottom:
  play/stop; delete (hold it and press a clip); clear the pattern.
  Up/down: octave. Left/right: tempo.
- **Gamepads**: A plays/stops, the left stick bends pitch, the right trigger
  is the modulation wheel, the D-pad does octave and tempo. Every gamepad
  plugged in does the same.

### Without hardware

On Linux and macOS, a pretend Launchpad and synth can stand in for the real
ones:

```sh
cargo run -p trama-io --example fake_gear
```

Then, in another terminal:

```sh
cargo run -- play --pads "Fake LPX" --synth "Fake Synth" --no-gamepad
```

It plays a short pattern and reports what the synth received and how steady
the MIDI clock was.

## Documentation

- [Architecture](docs/architecture.md): how Trama is put together.
- [Decisions](docs/decisions.md): why it is built this way.
- [Contributing](CONTRIBUTING.md): building, testing and the project's
  conventions.
- Plans and ideas live in [GitHub issues](https://github.com/Antiloope/trama/issues).

## License

Trama is free software, licensed under the GNU General Public License,
version 3 or (at your option) any later version. See [LICENSE](LICENSE).
