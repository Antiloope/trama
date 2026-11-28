// SPDX-License-Identifier: GPL-3.0-or-later

//! The brain of Trama.
//!
//! The core owns the patch: the devices it knows, the modules, and the
//! connections between them. It is the only part allowed to change them or
//! the modules' state. Its job, over and over:
//!
//! 1. receive an `Input` and follow the connections from that control to
//!    the inputs of modules (or receive a `Command` for one input);
//! 2. let each of those modules handle it;
//! 3. send what the modules produce where their outputs are connected:
//!    engine commands ([`Output::Engine`]), MIDI to devices and the lights
//!    of controls ([`Output::Notify`]);
//! 4. when modules or connections change, describe the engine patch anew
//!    ([`Core::take_engine_patch`]).
//!
//! Every project has a main clock, [`MAIN_CLOCK`]; modules that follow a
//! tempo follow it unless their `clock` input is connected to another.
//!
//! The core has no devices, no audio and no threads of its own: every method
//! takes a message or a change and returns the messages to send. That makes
//! it plain logic, easy to test.

pub mod module;
pub mod modules;
pub mod music;
pub mod patch;

use std::collections::{BTreeSet, HashMap};

pub use module::{Direction, Port, PortKind, Shape};
use module::{Effect, Effects, EngineHalf, Module};
pub use modules::ModuleSettings;
use modules::clock::Clock;
use modules::gain::Gain;
use modules::looper::Looper;
use modules::step_sequencer::StepSequencer;
pub use patch::{Connection, Curve, PatchError, Point, RowButtons, Transform, Zone};
use trama_protocol::{
    Action, ClockDescription, Command, Control, DeviceId, EngineCommand, EngineEvent, EnginePatch,
    Input, Jack, Light, ModuleId, NodeId, NodeKind, Notification, StepPlayerDescription,
};

/// The project's main clock.
pub const MAIN_CLOCK: ModuleId = match ModuleId::new("clock") {
    Some(id) => id,
    None => panic!("not a name"),
};

/// Graph nodes for the sound card's channels, above every module's node.
const AUDIO_IN_NODES: u32 = 0xFFFE_0000;
const AUDIO_OUT_NODES: u32 = 0xFFFF_0000;

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Engine(EngineCommand),
    /// To every adapter.
    Notify(Notification),
}

struct Slot {
    id: ModuleId,
    /// Its node in the audio graph, if its engine half is one.
    node: NodeId,
    module: Box<dyn Module>,
}

pub struct Core {
    devices: Vec<DeviceId>,
    modules: Vec<Slot>,
    next_node: u32,
    connections: Vec<Connection>,
    /// Between `start` and `shutdown`, the core sends lights and MIDI.
    started: bool,
    /// What each lit control shows, so only changes are sent.
    shown: HashMap<(DeviceId, Control), Light>,
    engine_patch_changed: bool,
}

impl Core {
    /// A core with only the main clock, at `bpm`.
    pub fn new(bpm: f64) -> Core {
        Core {
            devices: Vec::new(),
            modules: vec![Slot {
                id: MAIN_CLOCK,
                node: NodeId(0),
                module: Box::new(Clock::new(MAIN_CLOCK, bpm)),
            }],
            next_node: 1,
            connections: Vec::new(),
            started: false,
            shown: HashMap::new(),
            engine_patch_changed: true,
        }
    }

    /// Adds `device` to the devices connections can use. Returns `false` if
    /// it was there already.
    pub fn add_device(&mut self, device: DeviceId) -> bool {
        if self.devices.contains(&device) {
            return false;
        }
        self.devices.push(device);
        true
    }

    pub fn devices(&self) -> &[DeviceId] {
        &self.devices
    }

    pub fn add_module(
        &mut self,
        id: ModuleId,
        settings: ModuleSettings,
    ) -> Result<Vec<Output>, PatchError> {
        if self.slot(id).is_some() {
            return Err(PatchError::DuplicateModule(id));
        }
        let node = NodeId(self.next_node);
        let invalid = |reason| PatchError::InvalidSettings { module: id, reason };
        let mut module: Box<dyn Module> = match settings {
            ModuleSettings::StepSequencer(settings) => {
                Box::new(StepSequencer::new(id, settings).map_err(invalid)?)
            }
            ModuleSettings::Gain(settings) => Box::new(Gain::new(node, settings).map_err(invalid)?),
            ModuleSettings::Looper(settings) => {
                Box::new(Looper::new(node, settings).map_err(invalid)?)
            }
        };
        self.next_node += 1;
        let mut out = Vec::new();
        if self.started {
            let mut effects = Effects::default();
            module.start(&mut effects);
            self.route(id, effects, &mut out);
        }
        self.modules.push(Slot { id, node, module });
        self.engine_patch_changed = true;
        Ok(out)
    }

    /// Removes module `id` and every connection it has. It leaves nothing
    /// hanging first: no bent pitch, no lit control.
    pub fn remove_module(&mut self, id: ModuleId) -> Result<Vec<Output>, PatchError> {
        if id == MAIN_CLOCK {
            return Err(PatchError::MainClock);
        }
        let index = self
            .modules
            .iter()
            .position(|s| s.id == id)
            .ok_or(PatchError::UnknownModule(id))?;
        let mut out = Vec::new();
        let mut effects = Effects::default();
        self.modules[index].module.shutdown(&mut effects);
        self.route(id, effects, &mut out);
        let (gone, kept) = self
            .connections
            .iter()
            .partition(|c| c.from.module_id() == Some(id) || c.to.module_id() == Some(id));
        self.connections = kept;
        for connection in gone {
            self.unlight(&connection, &mut out);
        }
        self.modules.remove(index);
        self.engine_patch_changed = true;
        Ok(out)
    }

    pub fn modules(&self) -> impl Iterator<Item = ModuleId> + '_ {
        self.modules.iter().map(|s| s.id)
    }

    /// Module `id`'s definition: its inputs and outputs.
    pub fn ports(&self, id: ModuleId) -> Option<&[Port]> {
        Some(self.slot(id)?.module.ports())
    }

    pub fn connect(&mut self, connection: Connection) -> Result<Vec<Output>, PatchError> {
        let kind = self.check(&connection)?;
        self.connections.push(connection);
        if matches!(kind, PortKind::Audio | PortKind::Notes | PortKind::Clock) {
            self.engine_patch_changed = true;
        }
        let mut out = Vec::new();
        self.refresh_lights(&mut out);
        Ok(out)
    }

    pub fn disconnect(&mut self, from: &Point, to: &Point) -> Result<Vec<Output>, PatchError> {
        let index = self
            .connections
            .iter()
            .position(|c| c.from == *from && c.to == *to)
            .ok_or(PatchError::NotConnected)?;
        if matches!(
            self.carries(&self.connections[index]),
            PortKind::Audio | PortKind::Notes | PortKind::Clock
        ) {
            self.engine_patch_changed = true;
        }
        let connection = self.connections.remove(index);
        let mut out = Vec::new();
        self.unlight(&connection, &mut out);
        Ok(out)
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    /// The engine patch, if modules or connections changed it since it was
    /// last taken. It carries every module's state, such as the patterns,
    /// so commands sent before it do not need sending again.
    pub fn take_engine_patch(&mut self) -> Option<EnginePatch> {
        if !self.engine_patch_changed {
            return None;
        }
        self.engine_patch_changed = false;
        Some(self.engine_patch())
    }

    /// What the modules show right now, as the notifications that would
    /// bring something that has seen none of them up to date: each clock's
    /// transport, each sequencer's steps and playhead.
    pub fn state(&self) -> Vec<Notification> {
        let mut out = Vec::new();
        for slot in &self.modules {
            slot.module.state(&mut out);
        }
        out
    }

    /// Everything to send when Trama starts: every light, and whatever each
    /// module sends first, such as a centered pitch bend.
    pub fn start(&mut self) -> Vec<Output> {
        self.started = true;
        let mut out = Vec::new();
        for index in 0..self.modules.len() {
            let mut effects = Effects::default();
            self.modules[index].module.start(&mut effects);
            self.route(self.modules[index].id, effects, &mut out);
        }
        self.refresh_lights(&mut out);
        out
    }

    /// Everything to send before Trama quits: clocks stop, synths are
    /// silenced, gains muted, lights turned off.
    pub fn shutdown(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        for index in 0..self.modules.len() {
            let mut effects = Effects::default();
            self.modules[index].module.shutdown(&mut effects);
            self.route(self.modules[index].id, effects, &mut out);
        }
        for ((device, control), light) in self.shown.drain() {
            if light != Light::OFF {
                out.push(Output::Notify(Notification::Light {
                    device,
                    control,
                    light: Light::OFF,
                }));
            }
        }
        self.started = false;
        out
    }

    pub fn handle_input(&mut self, input: &Input) -> Vec<Output> {
        let mut commands = Vec::new();
        for connection in &self.connections {
            let Point::Module { module, port } = connection.to else {
                continue;
            };
            let cell = match connection.from {
                Point::Control { device, control }
                    if device == input.device && control == input.control =>
                {
                    None
                }
                Point::Pads { device, zone } if device == input.device => {
                    match zone.cell(input.control) {
                        Some(cell) => Some(cell),
                        None => continue,
                    }
                }
                Point::RowButtons { device, rows } if device == input.device => {
                    match rows.cell(input.control) {
                        Some(cell) => Some(cell),
                        None => continue,
                    }
                }
                _ => continue,
            };
            let action = match (connection.transform, input.action) {
                (Some(transform), Action::Move { value }) => Action::Move {
                    value: transform.apply(value),
                },
                (_, action) => action,
            };
            commands.push(Command {
                module,
                input: port,
                cell,
                action,
            });
        }
        let mut out = Vec::new();
        for command in commands {
            self.deliver(command, &mut out);
        }
        self.refresh_lights(&mut out);
        out
    }

    /// Hands `command` to its module, as if a connected control had sent
    /// it. Ignored if the module has no such control input.
    pub fn handle_command(&mut self, command: Command) -> Vec<Output> {
        let mut out = Vec::new();
        let is_control_input = self.ports(command.module).is_some_and(|ports| {
            ports.iter().any(|p| {
                p.name == command.input
                    && p.direction == Direction::Input
                    && p.kind == PortKind::Control
            })
        });
        if is_control_input {
            self.deliver(command, &mut out);
            self.refresh_lights(&mut out);
        }
        out
    }

    pub fn handle_engine_event(&mut self, event: EngineEvent) -> Vec<Output> {
        let mut out = Vec::new();
        let concerned: Vec<ModuleId> = match event {
            EngineEvent::StepStarted { player, .. } => vec![player],
            EngineEvent::Track { node, .. } | EngineEvent::Clip { node, .. } => self
                .modules
                .iter()
                .filter(|s| s.node == node)
                .map(|s| s.id)
                .collect(),
            EngineEvent::Started { clock } | EngineEvent::Stopped { clock } => self
                .modules
                .iter()
                .map(|s| s.id)
                .filter(|&id| id == clock || self.clock_of(id) == Some(clock))
                .collect(),
        };
        for id in concerned {
            if let Some(slot) = self.modules.iter_mut().find(|s| s.id == id) {
                let mut effects = Effects::default();
                slot.module.engine_event(event, &mut effects);
                self.route(id, effects, &mut out);
            }
        }
        self.refresh_lights(&mut out);
        out
    }

    fn slot(&self, id: ModuleId) -> Option<&Slot> {
        self.modules.iter().find(|s| s.id == id)
    }

    fn deliver(&mut self, command: Command, out: &mut Vec<Output>) {
        let Some(slot) = self.modules.iter_mut().find(|s| s.id == command.module) else {
            return;
        };
        let mut effects = Effects::default();
        slot.module
            .receive(command.input, command.cell, command.action, &mut effects);
        self.route(command.module, effects, out);
    }

    /// Sends what module `id` produced where its outputs are connected.
    fn route(&self, id: ModuleId, effects: Effects, out: &mut Vec<Output>) {
        for effect in effects.items {
            match effect {
                Effect::Engine(command) => out.push(Output::Engine(command)),
                Effect::Notify(notification) => out.push(Output::Notify(notification)),
                Effect::Midi { output, message } if self.started => {
                    for connection in &self.connections {
                        if let (Point::Module { module, port }, Point::Midi(device)) =
                            (connection.from, connection.to)
                        {
                            if module == id && port == output {
                                out.push(Output::Notify(Notification::Midi { device, message }));
                            }
                        }
                    }
                }
                Effect::Midi { .. } => {}
            }
        }
    }

    /// The clock module `id` follows, if it follows one: the one connected
    /// to its clock input, or the main clock.
    fn clock_of(&self, id: ModuleId) -> Option<ModuleId> {
        let input = self
            .slot(id)?
            .module
            .ports()
            .iter()
            .find(|p| p.direction == Direction::Input && p.kind == PortKind::Clock)?;
        let connected = self.connections.iter().find_map(|c| match (c.from, c.to) {
            (Point::Module { module: clock, .. }, Point::Module { module, port })
                if module == id && port == input.name =>
            {
                Some(clock)
            }
            _ => None,
        });
        Some(connected.unwrap_or(MAIN_CLOCK))
    }

    /// Sends the light of every lit control that changed.
    fn refresh_lights(&mut self, out: &mut Vec<Output>) {
        if !self.started {
            return;
        }
        for connection in &self.connections {
            let Point::Module { module, port } = connection.from else {
                continue;
            };
            let Some(slot) = self.modules.iter().find(|s| s.id == module) else {
                continue;
            };
            let lit: Vec<(DeviceId, Control, Light)> = match connection.to {
                Point::Control { device, control } => {
                    vec![(device, control, slot.module.light(port, None))]
                }
                Point::Pads { device, zone } => zone
                    .pads()
                    .map(|pad| {
                        let light = zone
                            .cell(pad)
                            .map_or(Light::OFF, |cell| slot.module.light(port, Some(cell)));
                        (device, pad, light)
                    })
                    .collect(),
                Point::RowButtons { device, rows } => rows
                    .buttons()
                    .map(|button| {
                        let light = rows
                            .cell(button)
                            .map_or(Light::OFF, |cell| slot.module.light(port, Some(cell)));
                        (device, button, light)
                    })
                    .collect(),
                _ => continue,
            };
            for (device, control, light) in lit {
                if self.shown.get(&(device, control)) != Some(&light) {
                    self.shown.insert((device, control), light);
                    out.push(Output::Notify(Notification::Light {
                        device,
                        control,
                        light,
                    }));
                }
            }
        }
    }

    /// Turns off the controls a connection that is going away lit.
    fn unlight(&mut self, connection: &Connection, out: &mut Vec<Output>) {
        if connection.from.module_id().is_none() {
            return;
        }
        let Some(device) = connection.to.device() else {
            return;
        };
        for control in connection.to.controls() {
            if let Some(light) = self.shown.remove(&(device, control)) {
                if light != Light::OFF {
                    out.push(Output::Notify(Notification::Light {
                        device,
                        control,
                        light: Light::OFF,
                    }));
                }
            }
        }
    }

    fn port(
        &self,
        id: ModuleId,
        name: &'static str,
        direction: Direction,
    ) -> Result<Port, PatchError> {
        let slot = self.slot(id).ok_or(PatchError::UnknownModule(id))?;
        slot.module
            .ports()
            .iter()
            .find(|p| p.name == name && p.direction == direction)
            .copied()
            .ok_or(PatchError::NoSuchPort {
                module: id,
                port: name,
            })
    }

    /// What an existing connection carries.
    fn carries(&self, connection: &Connection) -> PortKind {
        let port = match (connection.from, connection.to) {
            (Point::Module { module, port }, _) => self.port(module, port, Direction::Output),
            (_, Point::Module { module, port }) => self.port(module, port, Direction::Input),
            _ => return PortKind::Audio,
        };
        port.map_or(PortKind::Audio, |p| p.kind)
    }

    /// Whether `connection` can join the patch, and what it would carry.
    fn check(&self, connection: &Connection) -> Result<PortKind, PatchError> {
        let Connection {
            from,
            to,
            transform,
        } = *connection;
        let source = self.end(from, Direction::Output)?;
        let destination = self.end(to, Direction::Input)?;
        let cannot = PatchError::CannotConnect { from, to };
        let same_shape = |a: Shape, b: Shape| {
            matches!(
                (a, b),
                (Shape::One, Shape::One) | (Shape::Grid { .. }, Shape::Grid { .. })
            )
        };
        let kind = match (source, destination) {
            (End::Controls(shape), End::Port(input))
                if input.kind == PortKind::Control && same_shape(shape, input.shape) =>
            {
                PortKind::Control
            }
            (End::Port(output), End::Lit(shape))
                if output.kind == PortKind::Lights && same_shape(output.shape, shape) =>
            {
                PortKind::Lights
            }
            (End::Port(output), End::Midi)
                if matches!(output.kind, PortKind::Notes | PortKind::Clock) =>
            {
                output.kind
            }
            (End::Audio, End::Audio) => PortKind::Audio,
            (End::Audio, End::Port(input)) if input.kind == PortKind::Audio => PortKind::Audio,
            (End::Port(output), End::Audio) if output.kind == PortKind::Audio => PortKind::Audio,
            (End::Port(output), End::Port(input))
                if output.kind == input.kind
                    && matches!(output.kind, PortKind::Audio | PortKind::Clock) =>
            {
                output.kind
            }
            _ => return Err(cannot),
        };

        if let Some(transform) = transform {
            if kind != PortKind::Control || !transform.is_valid() {
                return Err(PatchError::InvalidTransform);
            }
        }
        if self
            .connections
            .iter()
            .any(|c| c.from == from && c.to == to)
        {
            return Err(PatchError::AlreadyConnected);
        }
        // An input and an output may share a name, so a port is only taken
        // by a connection at the same end.
        for (point, end, is_from) in [(from, source, true), (to, destination, false)] {
            if let (Point::Module { module, port }, End::Port(definition)) = (point, end) {
                let taken = self.connections.iter().any(|c| {
                    let same_end = if is_from { c.from } else { c.to };
                    same_end == point
                });
                if !definition.several && taken {
                    return Err(PatchError::PortTaken { module, port });
                }
            }
        }
        if kind == PortKind::Lights {
            self.check_light_owner(to)?;
        }
        if kind == PortKind::Audio {
            if let (Some(source), Some(destination)) = (from.module_id(), to.module_id()) {
                if source == destination || self.audio_reaches(destination, source) {
                    return Err(PatchError::Loop);
                }
            }
        }
        Ok(kind)
    }

    /// What `point` is at one end of a connection: the end that produces
    /// (`Output`) or the end that receives (`Input`).
    fn end(&self, point: Point, direction: Direction) -> Result<End, PatchError> {
        if let Some(device) = point.device() {
            if !self.devices.contains(&device) {
                return Err(PatchError::UnknownDevice(device));
            }
        }
        match point {
            Point::Pads { zone, .. } if zone.rows == 0 || zone.columns == 0 => {
                return Err(PatchError::InvalidZone);
            }
            Point::RowButtons { rows, .. } if rows.count == 0 => {
                return Err(PatchError::InvalidZone);
            }
            _ => {}
        }
        let shape = |point: Point| match point {
            Point::Pads { zone, .. } => Shape::Grid {
                rows: zone.rows,
                columns: zone.columns,
            },
            Point::RowButtons { rows, .. } => Shape::Grid {
                rows: rows.count,
                columns: 1,
            },
            _ => Shape::One,
        };
        let wrong_way = || PatchError::CannotConnect {
            from: point,
            to: point,
        };
        Ok(match (point, direction) {
            (Point::Module { module, port }, direction) => {
                End::Port(self.port(module, port, direction)?)
            }
            (
                Point::Control { .. } | Point::Pads { .. } | Point::RowButtons { .. },
                Direction::Output,
            ) => End::Controls(shape(point)),
            (
                Point::Control { .. } | Point::Pads { .. } | Point::RowButtons { .. },
                Direction::Input,
            ) => End::Lit(shape(point)),
            (Point::Midi(_), Direction::Input) => End::Midi,
            (Point::AudioIn(_), Direction::Output) | (Point::AudioOut(_), Direction::Input) => {
                End::Audio
            }
            (Point::Midi(_) | Point::AudioIn(_) | Point::AudioOut(_), _) => return Err(wrong_way()),
        })
    }

    fn check_light_owner(&self, to: Point) -> Result<(), PatchError> {
        let Some(device) = to.device() else {
            return Ok(());
        };
        let wanted = to.controls();
        for connection in &self.connections {
            if connection.from.module_id().is_none() || connection.to.device() != Some(device) {
                continue;
            }
            if let Some(&control) = connection.to.controls().iter().find(|c| wanted.contains(c)) {
                return Err(PatchError::LightTaken { device, control });
            }
        }
        Ok(())
    }

    /// Whether audio from module `from` reaches module `to` through
    /// connections between modules.
    fn audio_reaches(&self, from: ModuleId, to: ModuleId) -> bool {
        let mut seen = BTreeSet::new();
        let mut waiting = vec![from];
        while let Some(module) = waiting.pop() {
            if module == to {
                return true;
            }
            if !seen.insert(module) {
                continue;
            }
            for connection in &self.connections {
                if connection.from.module_id() == Some(module) {
                    if let Some(next) = connection.to.module_id() {
                        if self.carries(connection) == PortKind::Audio {
                            waiting.push(next);
                        }
                    }
                }
            }
        }
        false
    }

    fn engine_patch(&self) -> EnginePatch {
        let mut patch = EnginePatch::default();
        for slot in &self.modules {
            let midi_to = |kind: PortKind| -> Vec<DeviceId> {
                self.connections
                    .iter()
                    .filter_map(|c| match (c.from, c.to) {
                        (Point::Module { module, port }, Point::Midi(device))
                            if module == slot.id
                                && self
                                    .port(module, port, Direction::Output)
                                    .is_ok_and(|p| p.kind == kind) =>
                        {
                            Some(device)
                        }
                        _ => None,
                    })
                    .collect()
            };
            match slot.module.engine_half() {
                EngineHalf::None => {}
                EngineHalf::Clock { bpm, beats_per_bar } => patch.clocks.push(ClockDescription {
                    id: slot.id,
                    bpm,
                    beats_per_bar,
                    midi_to: midi_to(PortKind::Clock),
                }),
                EngineHalf::StepPlayer {
                    channel,
                    pulses_per_step,
                    gate_pulses,
                    pattern,
                } => patch.step_players.push(StepPlayerDescription {
                    id: slot.id,
                    clock: self.clock_of(slot.id).unwrap_or(MAIN_CLOCK),
                    channel,
                    pulses_per_step,
                    gate_pulses,
                    pattern,
                    midi_to: midi_to(PortKind::Notes),
                }),
                EngineHalf::Node(kind) => {
                    patch.graph.add(slot.node, kind);
                }
                EngineHalf::Looper {
                    tracks,
                    scenes,
                    channels,
                    seconds,
                    synced,
                } => {
                    let clock = synced.then(|| self.clock_of(slot.id).unwrap_or(MAIN_CLOCK));
                    patch.graph.add(
                        slot.node,
                        NodeKind::Looper {
                            tracks,
                            scenes,
                            channels,
                            seconds,
                            clock,
                        },
                    );
                }
            }
        }

        let mut card_nodes = BTreeSet::new();
        for connection in &self.connections {
            if self.carries(connection) != PortKind::Audio {
                continue;
            }
            let (Some(from), Some(to)) = (
                self.jack(connection.from, Direction::Output),
                self.jack(connection.to, Direction::Input),
            ) else {
                continue;
            };
            for (point, jack) in [(connection.from, from), (connection.to, to)] {
                let kind = match point {
                    Point::AudioIn(channel) => NodeKind::AudioInput { channel },
                    Point::AudioOut(channel) => NodeKind::AudioOutput { channel },
                    _ => continue,
                };
                if card_nodes.insert(jack.node) {
                    patch.graph.add(jack.node, kind);
                }
            }
            patch.graph.connect(from, to);
        }
        patch
    }

    /// The graph jack an audio point is.
    fn jack(&self, point: Point, direction: Direction) -> Option<Jack> {
        match point {
            Point::AudioIn(channel) => Some(NodeId(AUDIO_IN_NODES + u32::from(channel)).jack(0)),
            Point::AudioOut(channel) => Some(NodeId(AUDIO_OUT_NODES + u32::from(channel)).jack(0)),
            Point::Module { module, port } => {
                let slot = self.slot(module)?;
                let index = slot
                    .module
                    .ports()
                    .iter()
                    .filter(|p| p.direction == direction && p.kind == PortKind::Audio)
                    .position(|p| p.name == port)?;
                Some(slot.node.jack(index as u16))
            }
            _ => None,
        }
    }
}

/// One end of a connection, as far as checking it goes.
#[derive(Clone, Copy)]
enum End {
    /// Controls of a device, which send presses and values.
    Controls(Shape),
    /// Controls of a device, which are lit.
    Lit(Shape),
    Midi,
    /// A channel of the sound card.
    Audio,
    Port(Port),
}
