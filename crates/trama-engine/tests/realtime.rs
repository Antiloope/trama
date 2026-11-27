// SPDX-License-Identifier: GPL-3.0-or-later

//! The audio thread must never allocate memory. This test replaces the
//! memory allocator with one that aborts the test if anything allocates
//! inside `assert_no_alloc`.

use std::time::Instant;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use trama_engine::{Engine, EngineConfig};
use trama_protocol::{EngineCommand, StepNote};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn processing_never_allocates() {
    let mut engine = Engine::new(EngineConfig::new(48_000));
    let mut buffer = vec![0.0_f32; 512 * 2];
    let mut send = |command| engine.controller.send(command).unwrap();
    for step in 0..8 {
        send(EngineCommand::SetStep {
            step,
            note: Some(StepNote {
                pitch: 40 + step,
                velocity: 100,
            }),
        });
    }
    send(EngineCommand::SetTempo { bpm: 174.0 });
    send(EngineCommand::Play);

    let processor = &mut engine.processor;
    assert_no_alloc(|| {
        // About a minute of audio: enough to fill the MIDI queue, which
        // nobody empties here, so dropping messages is also covered.
        for _ in 0..6000 {
            processor.process(&mut buffer, 2, Instant::now());
        }
    });
    assert!(processor.dropped_messages() > 0);
}
