# Trama

Trama connects synths, MIDI controllers and gamepads and plays them together,
in time with the sound card, without a DAW. It is free software written in
Rust, and runs on Linux and macOS.

It runs an 8-step sequencer: a grid controller edits the pattern and shows
the playhead, a gamepad adds pitch bend and modulation, and a synth with a
MIDI input plays the notes and follows Trama's MIDI clock.

## Supported devices

| Kind             | What works                                                            |
|------------------|-----------------------------------------------------------------------|
| Sound cards      | any output the system offers (PipeWire or ALSA on Linux, CoreAudio on macOS) |
| Synths           | anything with a MIDI input: hardware, or a program on a virtual MIDI port |
| Grid controllers | Novation Launchpad X                                                  |
| Gamepads         | those gilrs recognises (evdev on Linux, IOKit on macOS)               |

## Build

Install Rust from [rustup.rs](https://rustup.rs) and the system libraries:

- Debian and Ubuntu: `sudo apt install pkg-config libasound2-dev libpipewire-0.3-dev libclang-dev libudev-dev`
- Fedora: `sudo dnf install alsa-lib-devel pipewire-devel clang-devel systemd-devel`
- macOS: `xcode-select --install`

## Run

See what Trama can find (audio outputs, MIDI ports, gamepads):

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
| `--audio <name>`   | default    | sound card (it is the clock, even though it stays silent) |

Controls:

- **Grid**: each column is a step, each row a scale degree (root at the
  bottom). The button beside the top row: play/stop; beside the bottom row:
  clear. Up/down: octave. Left/right: tempo.
- **Gamepad**: A plays/stops, the left stick bends pitch, the right trigger is
  the modulation wheel, the D-pad does octave and tempo.

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
