// SPDX-License-Identifier: GPL-3.0-or-later

//! The patch `trama play` builds in code: a step sequencer and loopers
//! sharing a grid, a sequencer played from every gamepad and sending its
//! notes and the main clock to a synth, and the sound card's inputs
//! monitored through a gain for each output channel they reach.

use std::collections::BTreeSet;

use trama_core::modules::gain::GainSettings;
use trama_core::modules::looper::{LoopTiming, LooperSettings};
use trama_core::modules::step_sequencer::StepSequencerSettings;
use trama_core::{
    Connection, Core, Direction, MAIN_CLOCK, ModuleSettings, PatchError, Point, PortKind,
    RowButtons, Zone,
};
use trama_protocol::{Control, DeviceId, ModuleId};

pub const SEQUENCER: ModuleId = module("sequencer");
pub const LOOPER: ModuleId = module("looper");
pub const FREE_LOOPER: ModuleId = module("free-looper");
pub const PADS: DeviceId = device("pads");
pub const SYNTH: DeviceId = device("synth");

/// The rows and columns of the grid's pads.
const GRID_SIZE: u8 = 8;

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

/// How the grid's 8 × 8 pads are shared. The top `looper_rows` rows are
/// clips, a scene per row: the left columns on the main clock, the right
/// `free_columns` free. The rows below are the sequencer's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// 0 to 7: the sequencer keeps at least one row.
    pub looper_rows: u8,
    /// 0 to 8.
    pub free_columns: u8,
}

/// A looper of the layout: its module, how it keeps time, and its columns.
struct LooperPlace {
    id: ModuleId,
    timing: LoopTiming,
    column: u8,
    columns: u8,
}

impl Layout {
    pub fn sequencer_rows(self) -> u8 {
        GRID_SIZE - self.looper_rows
    }

    fn loopers(self) -> Vec<LooperPlace> {
        if self.looper_rows == 0 {
            return Vec::new();
        }
        let free = self.free_columns.min(GRID_SIZE);
        let synced = GRID_SIZE - free;
        [
            (LOOPER, LoopTiming::Synced, 0, synced),
            (FREE_LOOPER, LoopTiming::Free, synced, free),
        ]
        .into_iter()
        .filter(|&(_, _, _, columns)| columns > 0)
        .map(|(id, timing, column, columns)| LooperPlace {
            id,
            timing,
            column,
            columns,
        })
        .collect()
    }
}

/// The sequencer, with as many rows as the layout leaves it.
pub fn add_sequencer(
    core: &mut Core,
    settings: StepSequencerSettings,
    layout: Layout,
) -> Result<(), PatchError> {
    let settings = StepSequencerSettings {
        rows: layout.sequencer_rows(),
        ..settings
    };
    core.add_module(SEQUENCER, ModuleSettings::StepSequencer(settings))?;
    Ok(())
}

/// The layout's loopers, each track with `channels` channels.
pub fn add_loopers(core: &mut Core, layout: Layout, channels: u8) -> Result<(), PatchError> {
    for looper in layout.loopers() {
        core.add_module(
            looper.id,
            ModuleSettings::Looper(LooperSettings {
                tracks: looper.columns,
                scenes: layout.looper_rows,
                channels,
                timing: looper.timing,
                ..LooperSettings::default()
            }),
        )?;
    }
    Ok(())
}

/// Every track of every looper records the input channels of `cables` and
/// plays to their output channels, one cable per channel of the track,
/// counting channels from 1.
pub fn connect_loop_audio(
    core: &mut Core,
    layout: Layout,
    cables: &[(u16, u16)],
) -> Result<(), PatchError> {
    let channels = cables.len().max(1);
    for looper in layout.loopers() {
        let audio = |direction| -> Vec<&'static str> {
            core.ports(looper.id)
                .unwrap_or_default()
                .iter()
                .filter(|p| p.kind == PortKind::Audio && p.direction == direction)
                .map(|p| p.name)
                .collect()
        };
        let (inputs, outputs) = (audio(Direction::Input), audio(Direction::Output));
        for (index, (input, output)) in inputs.iter().zip(&outputs).enumerate() {
            let Some(&(from, to)) = cables.get(index % channels) else {
                continue;
            };
            core.connect(Connection::new(
                Point::AudioIn(from - 1),
                Point::module(looper.id, input),
            ))?;
            core.connect(Connection::new(
                Point::module(looper.id, output),
                Point::AudioOut(to - 1),
            ))?;
        }
    }
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

/// The grid as `layout` shares it. The buttons beside the looper's rows
/// launch scenes. Beside the sequencer's rows, from the bottom: play and
/// stop, delete (held, with a clip), clear the pattern, as far as there
/// are rows. Up and down move the octave, left and right the tempo. Each
/// control shows the light of what it does.
pub fn connect_pads(core: &mut Core, pads: DeviceId, layout: Layout) -> Result<(), PatchError> {
    core.add_device(pads);
    let both_ways = |core: &mut Core, control: Point, port: Point, lit: bool| {
        core.connect(Connection::new(control, port))?;
        if lit {
            core.connect(Connection::new(port, control))?;
        }
        Ok::<(), PatchError>(())
    };
    let sequencer_rows = layout.sequencer_rows();
    let grid = Point::Pads {
        device: pads,
        zone: Zone {
            row: 0,
            column: 0,
            rows: sequencer_rows,
            columns: GRID_SIZE,
        },
    };
    both_ways(core, grid, Point::module(SEQUENCER, "grid"), true)?;

    let loopers = layout.loopers();
    let scenes = Point::RowButtons {
        device: pads,
        rows: RowButtons {
            row: sequencer_rows,
            count: layout.looper_rows,
        },
    };
    for (index, looper) in loopers.iter().enumerate() {
        let clips = Point::Pads {
            device: pads,
            zone: Zone {
                row: sequencer_rows,
                column: looper.column,
                rows: layout.looper_rows,
                columns: looper.columns,
            },
        };
        both_ways(core, clips, Point::module(looper.id, "clips"), true)?;
        // One looper lights the buttons both of them listen to.
        both_ways(core, scenes, Point::module(looper.id, "scenes"), index == 0)?;
    }

    let mut functions: Vec<Vec<(ModuleId, &'static str)>> = vec![vec![(MAIN_CLOCK, "play")]];
    if !loopers.is_empty() {
        functions.push(loopers.iter().map(|l| (l.id, "delete")).collect());
    }
    functions.push(vec![(SEQUENCER, "clear")]);
    for (row, ports) in functions
        .into_iter()
        .enumerate()
        .take(usize::from(sequencer_rows))
    {
        let control = Point::control(pads, Control::RowButton { row: row as u8 });
        for (index, (module, port)) in ports.into_iter().enumerate() {
            both_ways(core, control, Point::module(module, port), index == 0)?;
        }
    }
    for (control, module, port) in [
        (Control::Button("up"), SEQUENCER, "octave_up"),
        (Control::Button("down"), SEQUENCER, "octave_down"),
        (Control::Button("left"), MAIN_CLOCK, "tempo_down"),
        (Control::Button("right"), MAIN_CLOCK, "tempo_up"),
    ] {
        both_ways(
            core,
            Point::control(pads, control),
            Point::module(module, port),
            true,
        )?;
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

    const SPLIT: Layout = Layout {
        looper_rows: 4,
        free_columns: 2,
    };

    fn whole_patch(layout: Layout) -> Core {
        let mut core = Core::new(120.0);
        add_sequencer(&mut core, StepSequencerSettings::default(), layout).unwrap();
        add_loopers(&mut core, layout, 2).unwrap();
        connect_loop_audio(&mut core, layout, &[(1, 1), (2, 2)]).unwrap();
        Monitoring::add(&mut core, &[(1, 1), (1, 2)], Some(-6.0)).unwrap();
        connect_synth(&mut core, SYNTH).unwrap();
        connect_pads(&mut core, PADS, layout).unwrap();
        for gamepad in ["gamepad-1", "gamepad-2"] {
            connect_gamepad(&mut core, DeviceId::new(gamepad).unwrap()).unwrap();
        }
        core
    }

    #[test]
    fn the_whole_patch_runs() {
        let mut core = whole_patch(SPLIT);
        let patch = core.take_engine_patch().unwrap();
        assert_eq!(patch.clocks[0].midi_to, vec![SYNTH]);
        assert_eq!(patch.step_players[0].midi_to, vec![SYNTH]);
        assert_eq!(patch.step_players[0].pattern.len(), 8);
        let mut engine = Engine::new(EngineConfig::new(48_000));
        engine.controller.set_patch(&patch).unwrap();
        // 64 pads, 4 scene buttons, play, delete, clear and 4 arrows, lit
        // once Trama starts, and the pitch bend centered.
        assert_eq!(core.start().len(), 64 + 4 + 3 + 4 + 1);
    }

    #[test]
    fn the_grid_is_shared_as_the_layout_says() {
        let core = whole_patch(SPLIT);
        let ports = |id| core.ports(id).unwrap().to_vec();
        let grid = |ports: &[trama_core::Port], name| {
            ports
                .iter()
                .find(|p| p.name == name && p.direction == Direction::Input)
                .map(|p| p.shape)
        };
        use trama_core::Shape;
        assert_eq!(
            grid(&ports(SEQUENCER), "grid"),
            Some(Shape::Grid {
                rows: 4,
                columns: 8
            })
        );
        assert_eq!(
            grid(&ports(LOOPER), "clips"),
            Some(Shape::Grid {
                rows: 4,
                columns: 6
            })
        );
        assert_eq!(
            grid(&ports(FREE_LOOPER), "clips"),
            Some(Shape::Grid {
                rows: 4,
                columns: 2
            })
        );
        // Every stereo track records inputs 1 and 2 and plays to outputs
        // 1 and 2.
        let audio = core
            .connections()
            .iter()
            .filter(|c| {
                matches!(c.from, Point::AudioIn(_)) && c.to.to_string().starts_with("looper")
            })
            .count();
        assert_eq!(audio, 6 * 2);

        let only_sequencer = whole_patch(Layout {
            looper_rows: 0,
            free_columns: 0,
        });
        assert_eq!(
            only_sequencer.modules().count(),
            1 + 1 + 2,
            "clock, sequencer, gains"
        );
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
