# Trama

Trama connects synths, MIDI controllers and gamepads and plays them together,
in time with the sound card, without a DAW. It is free software written in
Rust, and runs on Linux and macOS.

It runs an 8-step sequencer: a grid controller edits the pattern and shows
the playhead, gamepads add pitch bend and modulation, and a synth with a
MIDI input plays the notes and follows Trama's MIDI clock. What reaches the
sound card's inputs, a synth's audio or a microphone, can play through
Trama to its outputs.

Inside, the sequencer is a module, connected to the devices the way a
patchbay connects gear; [the architecture](docs/architecture.md) explains
modules and connections.

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

The input and the output must run on one clock:

- **Linux with PipeWire**: any input and output.
- **Linux without PipeWire**: the input of the output device itself.
- **macOS**: the input of the output device itself. To use, say, the
  built-in microphone with headphones, join both in an aggregate device in
  Audio MIDI Setup and choose it with `--audio` and `--audio-in`. macOS
  also asks whether the terminal may use the microphone; until it may, the
  input is silent, and Trama says so.

Between input and output there is a delay of a few blocks. At 48 kHz a
block of 1024 frames lasts 21 ms; `--block-size 128` asks for blocks of
under 3 ms, at the cost of more CPU. Trama reports underruns and overruns
(input that came too late or too early) as they happen.

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
| `--block-size <n>` | the system's | frames per audio block; smaller is less delay       |

Controls:

- **Grid**: each column is a step, each row a scale degree (root at the
  bottom). The button beside the top row: play/stop; beside the bottom row:
  clear. Up/down: octave. Left/right: tempo.
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
