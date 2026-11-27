// SPDX-License-Identifier: GPL-3.0-or-later

//! The patch `trama play` builds in code: a step sequencer on the main
//! clock, played from a grid and from every gamepad, sending its notes and
//! the main clock to a synth; and the sound card's inputs monitored through
//! a gain for each output channel they reach.

use std::collections::BTreeSet;

use trama_core::modules::gain::GainSettings;
use trama_core::modules::step_sequencer::StepSequencerSettings;
use trama_core::{Connection, Core, MAIN_CLOCK, ModuleSettings, PatchError, Point, Zone};
use trama_protocol::{Control, DeviceId, ModuleId};

pub const SEQUENCER: ModuleId = module("sequencer");
pub const PADS: DeviceId = device("pads");
pub const SYNTH: DeviceId = device("synth");

/// The pads the sequencer's grid uses: 8 × 8 from the bottom-left corner.
const GRID: Zone = Zone {
    row: 0,
    column: 0,
    rows: 8,
    columns: 8,
};

const fn module(name: &str) -> ModuleId {
    match ModuleId::new(name) {
        Some(id) => id,
        None => panic!("not a name"),
    }
}

const fn device(name: &str) -> DeviceId {
    match DeviceId::new(name) {
        Some(id) => id,
        None => panic!("not a name"),
    }
}

pub fn add_sequencer(core: &mut Core, settings: StepSequencerSettings) -> Result<(), PatchError> {
    core.add_module(SEQUENCER, ModuleSettings::StepSequencer(settings))?;
    Ok(())
}

/// The sequencer's notes and the main clock to `synth`.
pub fn connect_synth(core: &mut Core, synth: DeviceId) -> Result<(), PatchError> {
    core.add_device(synth);
    for (module, output) in [(SEQUENCER, "notes"), (MAIN_CLOCK, "clock")] {
        core.connect(Connection::new(
            Point::module(module, output),
            Point::Midi(synth),
        ))?;
    }
    Ok(())
}

/// The grid edits the pattern and shows it. The row button beside the top
/// row plays and stops, the one beside the bottom row clears; up and down
/// move the octave, left and right the tempo. Each control shows the light
/// of what it does.
pub fn connect_pads(core: &mut Core, pads: DeviceId) -> Result<(), PatchError> {
    core.add_device(pads);
    let grid = Point::Pads {
        device: pads,
        zone: GRID,
    };
    let sequencer_grid = Point::module(SEQUENCER, "grid");
    core.connect(Connection::new(grid, sequencer_grid))?;
    core.connect(Connection::new(sequencer_grid, grid))?;
    for (control, module, port) in [
        (Control::RowButton { row: 7 }, MAIN_CLOCK, "play"),
        (Control::RowButton { row: 0 }, SEQUENCER, "clear"),
        (Control::Button("up"), SEQUENCER, "octave_up"),
        (Control::Button("down"), SEQUENCER, "octave_down"),
        (Control::Button("left"), MAIN_CLOCK, "tempo_down"),
        (Control::Button("right"), MAIN_CLOCK, "tempo_up"),
    ] {
        let control = Point::control(pads, control);
        let port = Point::module(module, port);
        core.connect(Connection::new(control, port))?;
        core.connect(Connection::new(port, control))?;
    }
    Ok(())
}

/// `south` plays and stops, the D-pad does what the grid's arrows do, the
/// left stick bends the pitch and the right trigger is the modulation
/// wheel. Every gamepad connected this way plays the same sequencer.
pub fn connect_gamepad(core: &mut Core, gamepad: DeviceId) -> Result<(), PatchError> {
    core.add_device(gamepad);
    for (control, module, port) in [
        (Control::Button("south"), MAIN_CLOCK, "play"),
        (Control::Button("dpad_up"), SEQUENCER, "octave_up"),
        (Control::Button("dpad_down"), SEQUENCER, "octave_down"),
        (Control::Button("dpad_left"), MAIN_CLOCK, "tempo_down"),
        (Control::Button("dpad_right"), MAIN_CLOCK, "tempo_up"),
        (Control::Axis("left_stick_y"), SEQUENCER, "pitch_bend"),
        (Control::Axis("right_trigger"), SEQUENCER, "modulation"),
    ] {
        core.connect(Connection::new(
            Point::control(gamepad, control),
            Point::module(module, port),
        ))?;
    }
    Ok(())
}

/// The monitoring part of the patch, from `--connect` and `--monitor`.
pub struct Monitoring {
    /// Input and output channels, from 1, sorted and without repeats.
    pub cables: Vec<(u16, u16)>,
}

impl Monitoring {
    /// Each output channel named in `cables` gets a gain, `monitor-N` after
    /// the channel, and every input channel cabled to it goes through that
    /// gain. `cables` count channels from 1. The gains start at `db`
    /// decibels, or muted at 0 dB if there is no `db`.
    pub fn add(
        core: &mut Core,
        cables: &[(u16, u16)],
        db: Option<f32>,
    ) -> anyhow::Result<Monitoring> {
        let cables: BTreeSet<(u16, u16)> = cables.iter().copied().collect();
        let outputs: BTreeSet<u16> = cables.iter().map(|&(_, to)| to).collect();
        let settings = GainSettings {
            level_db: db.unwrap_or(0.0),
            muted: db.is_none(),
        };
        let mut gains = Vec::new();
        for &to in &outputs {
            let gain = ModuleId::new(&format!("monitor-{to}"))
                .ok_or_else(|| anyhow::anyhow!("no name for output {to}"))?;
            core.add_module(gain, ModuleSettings::Gain(settings))?;
            core.connect(Connection::new(
                Point::module(gain, "out"),
                Point::AudioOut(to - 1),
            ))?;
            gains.push(gain);
        }
        for &(from, to) in &cables {
            let gain = gains[outputs.iter().position(|o| *o == to).unwrap_or_default()];
            core.connect(Connection::new(
                Point::AudioIn(from - 1),
                Point::module(gain, "in"),
            ))?;
        }
        Ok(Monitoring {
            cables: cables.into_iter().collect(),
        })
    }

    pub fn describe(&self, db: Option<f32>) -> String {
        if self.cables.is_empty() {
            return "nothing connected (see --connect)".into();
        }
        let cables: Vec<String> = self
            .cables
            .iter()
            .map(|(from, to)| format!("{from}→{to}"))
            .collect();
        let volume = match db {
            Some(db) => format!("{db:+} dB"),
            None => "muted (turn it up with --monitor)".into(),
        };
        format!("{}, {volume}", cables.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use trama_engine::{Engine, EngineConfig};
    use trama_protocol::EnginePatch;

    use super::*;

    fn engine_with(core: &mut Core) -> Engine {
        let mut engine = Engine::new(EngineConfig::new(48_000));
        let patch: EnginePatch = core.take_engine_patch().unwrap();
        engine.controller.set_patch(&patch).unwrap();
        engine
    }

    /// Runs `frames` frames of `input` on every input channel and returns
    /// the last output frame.
    fn last_frame(engine: &mut Engine, input: &[f32], outputs: usize) -> Vec<f32> {
        let frames = 2048;
        let input: Vec<f32> = (0..frames).flat_map(|_| input.iter().copied()).collect();
        let mut output = vec![f32::NAN; frames * outputs];
        engine.processor.process(
            &input,
            input.len() / frames,
            &mut output,
            outputs,
            Instant::now(),
        );
        output[(frames - 1) * outputs..].to_vec()
    }

    #[test]
    fn the_whole_patch_runs() {
        let mut core = Core::new(120.0);
        add_sequencer(&mut core, StepSequencerSettings::default()).unwrap();
        Monitoring::add(&mut core, &[(1, 1), (1, 2)], Some(-6.0)).unwrap();
        connect_synth(&mut core, SYNTH).unwrap();
        connect_pads(&mut core, PADS).unwrap();
        for gamepad in ["gamepad-1", "gamepad-2"] {
            connect_gamepad(&mut core, DeviceId::new(gamepad).unwrap()).unwrap();
        }
        let patch = core.take_engine_patch().unwrap();
        assert_eq!(patch.clocks[0].midi_to, vec![SYNTH]);
        assert_eq!(patch.step_players[0].midi_to, vec![SYNTH]);
        let mut engine = Engine::new(EngineConfig::new(48_000));
        engine.controller.set_patch(&patch).unwrap();
        // 64 pads and 6 buttons, lit once Trama starts.
        assert_eq!(core.start().len(), 64 + 6 + 1);
    }

    #[test]
    fn monitoring_starts_muted() {
        let mut core = Core::new(120.0);
        Monitoring::add(&mut core, &[(1, 1), (1, 2)], None).unwrap();
        let mut engine = engine_with(&mut core);
        assert_eq!(last_frame(&mut engine, &[0.5], 2), vec![0.0, 0.0]);
    }

    #[test]
    fn monitoring_plays_each_cable_at_its_volume() {
        // A stereo pair, plus input 3 on both sides.
        let mut core = Core::new(120.0);
        let monitoring = Monitoring::add(
            &mut core,
            &[(1, 1), (2, 2), (3, 1), (3, 2), (1, 1)],
            Some(-6.0),
        )
        .unwrap();
        assert_eq!(monitoring.cables, vec![(1, 1), (2, 2), (3, 1), (3, 2)]);
        assert_eq!(core.modules().count(), 1 + 2, "the clock and two gains");
        let mut engine = engine_with(&mut core);
        let last = last_frame(&mut engine, &[0.1, 0.2, 0.4], 4);
        let half = 10_f32.powf(-6.0 / 20.0);
        assert!((last[0] - (0.1 + 0.4) * half).abs() < 1e-6);
        assert!((last[1] - (0.2 + 0.4) * half).abs() < 1e-6);
        assert_eq!(&last[2..], &[0.0, 0.0], "unconnected outputs stay silent");
    }

    #[test]
    fn nothing_connected_is_silence() {
        let mut core = Core::new(120.0);
        Monitoring::add(&mut core, &[], Some(0.0)).unwrap();
        assert_eq!(core.modules().count(), 1, "only the clock");
        let mut engine = engine_with(&mut core);
        assert_eq!(last_frame(&mut engine, &[0.5, 0.5], 2), vec![0.0, 0.0]);
    }

    #[test]
    fn shutting_down_mutes_monitoring() {
        let mut core = Core::new(120.0);
        Monitoring::add(&mut core, &[(1, 1)], Some(0.0)).unwrap();
        core.start();
        let mut engine = engine_with(&mut core);
        assert_eq!(last_frame(&mut engine, &[0.5], 1), vec![0.5]);
        for output in core.shutdown() {
            if let trama_core::Output::Engine(command) = output {
                engine.controller.send(command).unwrap();
            }
        }
        assert_eq!(last_frame(&mut engine, &[0.5], 1), vec![0.0]);
    }
}
