# Trama

Trama is a free-software instrument hub for making music with hardware and
code. It connects synths, MIDI controllers, gamepads, tablets and phones to a
sound engine written in Rust, so you can jam, loop, sample and record without
a DAW.

It runs on Linux and macOS.

**Status:** early skeleton. Nothing makes sound yet.

## Build and run

Install Rust from [rustup.rs](https://rustup.rs), then:

```sh
cargo run
```

Once audio, MIDI and gamepad support lands, you will also need:

- Fedora: `sudo dnf install alsa-lib-devel pipewire-devel clang-devel systemd-devel`
- macOS: `xcode-select --install`

## Documentation

- [Architecture](docs/architecture.md): how Trama is put together.
- [Decisions](docs/decisions.md): what we chose and why.

## License

Trama is free software, licensed under the GNU General Public License,
version 3 or (at your option) any later version. See [LICENSE](LICENSE).
