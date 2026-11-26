// SPDX-License-Identifier: GPL-3.0-or-later

//! A pretend Launchpad X and synth, to try Trama end to end without any
//! hardware (Linux and macOS).
//!
//! 1. Start it:
//!
//!    cargo run -p trama-io --example fake_gear
//!
//! 2. In another terminal, start Trama pointing at it:
//!
//!    cargo run -- play --pads "Fake LPX" --synth "Fake Synth" --no-gamepad
//!
//! It presses four pads, starts the sequencer, lets it run for a few
//! seconds, stops it, and reports what the synth received and how steady the
//! MIDI clock was. Quit Trama with Ctrl+C and it also checks that the
//! Launchpad was put back in its normal mode.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    fake::run()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("virtual MIDI ports only exist on Linux and macOS");
}

#[cfg(unix)]
mod fake {
    use std::error::Error;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    use midir::os::unix::{VirtualInput, VirtualOutput};
    use midir::{MidiInput, MidiOutput};
    use trama_io::midi::launchpad_x::{LIVE_MODE, PROGRAMMER_MODE};
    use trama_protocol::MidiMessage;

    /// Pads pressed before playing, as (row, column). In C2 minor, rows 0, 4
    /// and 2 are C2, G2 and E flat 2.
    const PADS: [(u8, u8); 4] = [(0, 0), (4, 2), (0, 4), (2, 6)];
    const EXPECTED_PITCHES: [u8; 4] = [36, 43, 36, 39];
    const PLAY_FOR: Duration = Duration::from_secs(4);
    const BPM: f64 = 120.0;

    pub fn run() -> Result<(), Box<dyn Error>> {
        let (lights_tx, lights) = mpsc::channel::<Vec<u8>>();
        let _lights_port = MidiInput::new("fake-gear")?.create_virtual(
            "Fake LPX MIDI",
            move |_, bytes, _| {
                let _ = lights_tx.send(bytes.to_vec());
            },
            (),
        )?;
        let (synth_tx, synth) = mpsc::channel::<(Instant, Vec<u8>)>();
        let _synth_port = MidiInput::new("fake-gear")?.create_virtual(
            "Fake Synth",
            move |_, bytes, _| {
                let _ = synth_tx.send((Instant::now(), bytes.to_vec()));
            },
            (),
        )?;
        let mut pads = MidiOutput::new("fake-gear")?.create_virtual("Fake LPX MIDI")?;

        println!("Fake Launchpad and synth ready. Start Trama with:");
        println!("  cargo run -- play --pads \"Fake LPX\" --synth \"Fake Synth\" --no-gamepad");
        wait_for(&lights, &PROGRAMMER_MODE, Duration::from_secs(120))
            .ok_or("Trama never put the Launchpad in programmer mode")?;
        println!("Trama connected and switched the Launchpad to programmer mode.");
        thread::sleep(Duration::from_millis(500));

        let mut press = |bytes: [u8; 3]| -> Result<(), Box<dyn Error>> {
            pads.send(&bytes)?;
            thread::sleep(Duration::from_millis(20));
            Ok(())
        };
        for (row, column) in PADS {
            let note = (row + 1) * 10 + column + 1;
            press([0x90, note, 127])?;
            press([0x90, note, 0])?;
        }
        let lit = drain(&lights).len();
        // Top-right side button: play.
        press([0xB0, 89, 127])?;
        press([0xB0, 89, 0])?;
        thread::sleep(PLAY_FOR);
        press([0xB0, 89, 127])?;
        press([0xB0, 89, 0])?;
        thread::sleep(Duration::from_millis(300));

        let received: Vec<(Instant, MidiMessage)> = drain(&synth)
            .into_iter()
            .filter_map(|(at, bytes)| MidiMessage::parse(&bytes).map(|m| (at, m)))
            .collect();
        report(&received, lit);

        println!("\nNow quit Trama with Ctrl+C...");
        if wait_for(&lights, &LIVE_MODE, Duration::from_secs(120)).is_some() {
            println!("ok: Trama put the Launchpad back in its normal mode.");
        } else {
            println!("FAIL: the Launchpad was never put back in its normal mode.");
        }
        Ok(())
    }

    fn report(received: &[(Instant, MidiMessage)], lit: usize) {
        let clocks: Vec<Instant> = received
            .iter()
            .filter(|(_, m)| *m == MidiMessage::Clock)
            .map(|(at, _)| *at)
            .collect();
        let pitches: Vec<u8> = received
            .iter()
            .filter_map(|(_, m)| match m {
                MidiMessage::NoteOn { note, .. } => Some(*note),
                _ => None,
            })
            .collect();
        let has = |wanted: MidiMessage| received.iter().any(|(_, m)| *m == wanted);

        println!("\nReport:");
        check(
            lit >= PADS.len(),
            &format!("{lit} light messages after pressing the pads"),
        );
        check(has(MidiMessage::Start), "synth got Start");
        check(has(MidiMessage::Stop), "synth got Stop");
        check(
            pitches.len() >= 4 && pitches[..4] == EXPECTED_PITCHES,
            &format!("notes {pitches:?} (expected to start with {EXPECTED_PITCHES:?})"),
        );
        let ons = pitches.len();
        let offs = received
            .iter()
            .filter(|(_, m)| matches!(m, MidiMessage::NoteOff { .. }))
            .count();
        check(ons == offs, &format!("{ons} note ons and {offs} note offs"));

        let expected = 60.0 / (BPM * 24.0);
        let gaps: Vec<f64> = clocks
            .windows(2)
            .map(|w| w[1].duration_since(w[0]).as_secs_f64())
            .collect();
        if gaps.is_empty() {
            check(false, "no MIDI clock");
            return;
        }
        let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
        let worst = gaps
            .iter()
            .map(|g| (g - expected).abs())
            .fold(0.0, f64::max);
        check(
            (mean - expected).abs() < 0.0005,
            &format!(
                "{} clock pulses, {:.3} ms apart on average (expected {:.3} ms)",
                clocks.len(),
                mean * 1000.0,
                expected * 1000.0
            ),
        );
        check(
            worst < 0.002,
            &format!("worst pulse off by {:.3} ms", worst * 1000.0),
        );
        let late = &gaps[gaps.len() / 2..];
        let late_mean = late.iter().sum::<f64>() / late.len() as f64;
        check(
            (late_mean - expected).abs() < 0.0002,
            &format!("second half: {:.3} ms apart on average", late_mean * 1000.0),
        );
    }

    fn check(ok: bool, what: &str) {
        println!("  {} {what}", if ok { "ok:  " } else { "FAIL:" });
    }

    fn wait_for(messages: &Receiver<Vec<u8>>, wanted: &[u8], timeout: Duration) -> Option<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            if messages.recv_timeout(left).ok()? == wanted {
                return Some(());
            }
        }
    }

    fn drain<T>(messages: &Receiver<T>) -> Vec<T> {
        messages.try_iter().collect()
    }
}
