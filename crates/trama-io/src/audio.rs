// SPDX-License-Identifier: GPL-3.0-or-later

//! The sound card. This adapter only opens an output and calls a render
//! function for every block; audio never passes through the core.
//!
//! On Linux cpal talks to PipeWire directly when it is running, and falls
//! back to ALSA. On macOS it uses CoreAudio.

use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, Stream, StreamConfig};

use crate::error::{Error, Result};

/// The audio system in use and its output devices.
pub struct AudioReport {
    pub host: String,
    /// Device names, with `true` for the default one.
    pub outputs: Vec<(String, bool)>,
}

pub fn list_outputs() -> Result<AudioReport> {
    let host = cpal::default_host();
    let default = host.default_output_device().map(|d| d.to_string());
    let outputs = host
        .output_devices()
        .map_err(|e| Error::context("listing audio outputs", e))?
        .map(|device| {
            let name = device.to_string();
            let is_default = default.as_deref() == Some(name.as_str());
            (name, is_default)
        })
        .collect();
    Ok(AudioReport {
        host: host.id().to_string(),
        outputs,
    })
}

/// An output device that has been chosen but not started.
pub struct OutputDevice {
    device: Device,
    config: StreamConfig,
    name: String,
}

impl OutputDevice {
    /// The default output, or the first one whose name contains `pattern`
    /// (ignoring case).
    pub fn find(pattern: Option<&str>) -> Result<OutputDevice> {
        let host = cpal::default_host();
        let device = match pattern {
            None => host
                .default_output_device()
                .ok_or_else(|| Error::new("there is no default audio output"))?,
            Some(pattern) => {
                let wanted = pattern.to_lowercase();
                host.output_devices()
                    .map_err(|e| Error::context("listing audio outputs", e))?
                    .find(|d| d.to_string().to_lowercase().contains(&wanted))
                    .ok_or_else(|| {
                        Error::new(format!(
                            "no audio output matches \"{pattern}\" (see `trama doctor`)"
                        ))
                    })?
            }
        };
        let name = device.to_string();
        let supported = device
            .default_output_config()
            .map_err(|e| Error::context(&format!("reading the settings of \"{name}\""), e))?;
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: BufferSize::Default,
        };
        Ok(OutputDevice {
            device,
            config,
            name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.config.channels
    }

    /// Starts the stream. `render` runs on the audio thread for every block:
    /// it receives the interleaved buffer to fill, the channel count and the
    /// moment the block is being computed. It must never allocate, lock or
    /// block.
    pub fn start<R>(self, mut render: R) -> Result<AudioOutput>
    where
        R: FnMut(&mut [f32], usize, Instant) + Send + 'static,
    {
        let channels = usize::from(self.config.channels);
        let stream = self
            .device
            .build_output_stream(
                self.config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    render(data, channels, Instant::now());
                },
                |error| eprintln!("audio: {error}"),
                None,
            )
            .map_err(|e| Error::context(&format!("opening \"{}\"", self.name), e))?;
        stream
            .play()
            .map_err(|e| Error::context(&format!("starting \"{}\"", self.name), e))?;
        Ok(AudioOutput { _stream: stream })
    }
}

/// A running audio output. Audio stops when this is dropped.
pub struct AudioOutput {
    _stream: Stream,
}
