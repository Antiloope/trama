// SPDX-License-Identifier: GPL-3.0-or-later

//! The sound card. This adapter opens an output, and an input when asked,
//! and calls a render function for every block with the input that
//! arrived; audio never passes through the core.
//!
//! cpal opens input and output as two streams. The input reaches the output
//! callback through a cushion, a ring buffer a few blocks deep. On PipeWire
//! any input and output run on one clock, because PipeWire runs them
//! together; elsewhere only the input of the output device itself does. An
//! input on a clock of its own keeps its device's sample rate and is
//! resampled to the output's, by a ratio the cushion's fill steers.
//!
//! On Linux cpal talks to PipeWire directly when it is running, and falls
//! back to ALSA. On macOS it uses CoreAudio.

#[cfg(test)]
mod clocks_apart;
mod cushion;
mod drift;
mod resample;

use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, Host, Stream, StreamConfig};
pub use cushion::InputStats;
use cushion::{Counters, CushionReader, CushionWriter};
use resample::InputResampler;

use crate::error::{Error, Result};

/// The most frames the render function gets at once. A longer block from
/// the sound card is rendered in pieces.
const RENDER_FRAMES: usize = 4096;

/// How many frames the cushion holds at most.
const CUSHION_FRAMES: usize = 32_768;

/// The audio system in use and its devices.
pub struct AudioReport {
    pub host: String,
    pub outputs: Vec<DeviceInfo>,
    pub inputs: Vec<DeviceInfo>,
}

pub struct DeviceInfo {
    pub name: String,
    /// The channels Trama opens on it; `None` if the device would not say.
    pub channels: Option<u16>,
    pub is_default: bool,
}

pub fn list_devices() -> Result<AudioReport> {
    let host = cpal::default_host();
    let default_output = host.default_output_device().map(|d| d.to_string());
    let default_input = host.default_input_device().map(|d| d.to_string());
    let outputs = host
        .output_devices()
        .map_err(|e| Error::context("listing audio outputs", e))?
        .map(|device| {
            let name = device.to_string();
            DeviceInfo {
                is_default: default_output.as_deref() == Some(name.as_str()),
                channels: device.default_output_config().ok().map(|c| c.channels()),
                name,
            }
        })
        .collect();
    let inputs = capture_devices(&host)?
        .map(|device| {
            let name = device.to_string();
            DeviceInfo {
                is_default: default_input.as_deref() == Some(name.as_str()),
                channels: device.default_input_config().ok().map(|c| c.channels()),
                name,
            }
        })
        .collect();
    Ok(AudioReport {
        host: host.id().to_string(),
        outputs,
        inputs,
    })
}

/// Whether the audio system runs every stream on one clock, whatever the
/// devices.
fn one_clock_for_all(host: &Host) -> bool {
    #[cfg(target_os = "linux")]
    {
        host.id() == cpal::HostId::PipeWire
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = host;
        false
    }
}

/// The devices that capture sound. PipeWire also offers each output as an
/// input that records what plays on it; those are left out, so Trama never
/// records itself.
fn capture_devices(host: &Host) -> Result<impl Iterator<Item = Device>> {
    let skip_outputs = one_clock_for_all(host);
    Ok(host
        .input_devices()
        .map_err(|e| Error::context("listing audio inputs", e))?
        .filter(move |device| !(skip_outputs && device.supports_output())))
}

/// The first device whose name contains `pattern`, ignoring case.
fn matching(mut devices: impl Iterator<Item = Device>, pattern: &str) -> Option<Device> {
    let wanted = pattern.to_lowercase();
    devices.find(|d| d.to_string().to_lowercase().contains(&wanted))
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
            Some(pattern) => matching(
                host.output_devices()
                    .map_err(|e| Error::context("listing audio outputs", e))?,
                pattern,
            )
            .ok_or_else(|| {
                Error::new(format!(
                    "no audio output matches \"{pattern}\" (see `trama doctor`)"
                ))
            })?,
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

    /// Asks for blocks of `frames` frames, for the output and for any input
    /// opened with it, instead of the system's choice. Opening fails if the
    /// device cannot use that size.
    pub fn set_block_size(&mut self, frames: u32) {
        self.config.buffer_size = BufferSize::Fixed(frames);
    }

    /// Starts the stream, and `input`'s if there is one. `render` runs on
    /// the audio thread for every block: it receives the interleaved input
    /// and its channel count (an empty block and 0 without an input), the
    /// interleaved output to fill and its channel count, and the moment the
    /// block is being computed. It must never allocate, lock or block.
    pub fn start<R>(self, input: Option<InputDevice>, mut render: R) -> Result<AudioOutput>
    where
        R: FnMut(&[f32], usize, &mut [f32], usize, Instant) + Send + 'static,
    {
        let (input_stream, mut reader, counters) = match input {
            Some(input) => {
                let (stream, reader, counters) = input.start()?;
                (Some(stream), Some(reader), Some(counters))
            }
            None => (None, None, None),
        };
        let input_channels = reader.as_ref().map_or(0, CushionReader::channels);
        let mut received = vec![0.0_f32; RENDER_FRAMES * input_channels];
        let output_channels = usize::from(self.config.channels);
        let sample_rate = f64::from(self.config.sample_rate);

        let stream = self
            .device
            .build_output_stream(
                self.config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let now = Instant::now();
                    let frames = data.len() / output_channels;
                    let mut done = 0;
                    while done < frames {
                        let len = (frames - done).min(RENDER_FRAMES);
                        // A piece's moment is when it would have started,
                        // so MIDI anchors and the cushion's fill stay true.
                        let at = now + Duration::from_secs_f64(done as f64 / sample_rate);
                        let piece = &mut received[..len * input_channels];
                        if let Some(reader) = reader.as_mut() {
                            reader.read(piece, at);
                        }
                        render(
                            piece,
                            input_channels,
                            &mut data[done * output_channels..(done + len) * output_channels],
                            output_channels,
                            at,
                        );
                        done += len;
                    }
                },
                |error| eprintln!("audio: {error}"),
                None,
            )
            .map_err(|e| Error::context(&format!("opening \"{}\"", self.name), e))?;
        stream
            .play()
            .map_err(|e| Error::context(&format!("starting \"{}\"", self.name), e))?;
        Ok(AudioOutput {
            _output: stream,
            _input: input_stream,
            counters,
        })
    }
}

/// An input device that has been chosen but not started. It opens with
/// every channel the device offers and the output's block size. On the
/// output's clock it opens at the output's sample rate; on a clock of its
/// own, at the rate its device runs at, and it is resampled.
pub struct InputDevice {
    device: Device,
    config: StreamConfig,
    name: String,
    /// The output's sample rate, when the input is resampled to it.
    resampled_to: Option<u32>,
}

impl InputDevice {
    /// The default input, or the first one whose name contains `pattern`
    /// (ignoring case), to play through `output`.
    pub fn find(pattern: Option<&str>, output: &OutputDevice) -> Result<InputDevice> {
        let host = cpal::default_host();
        let device = match pattern {
            None => host
                .default_input_device()
                .ok_or_else(|| Error::new("there is no default audio input"))?,
            Some(pattern) => matching(capture_devices(&host)?, pattern).ok_or_else(|| {
                Error::new(format!(
                    "no audio input matches \"{pattern}\" (see `trama doctor`)"
                ))
            })?,
        };
        let name = device.to_string();
        let shares_clock = one_clock_for_all(&host) || same_device(&device, &output.device);
        let supported = device
            .default_input_config()
            .map_err(|e| Error::context(&format!("reading the settings of \"{name}\""), e))?;
        // A device on a clock of its own keeps the rate it runs at: asking
        // for another would change it for every program using it.
        let sample_rate = if shares_clock {
            output.config.sample_rate
        } else {
            supported.sample_rate()
        };
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate,
            buffer_size: output.config.buffer_size,
        };
        Ok(InputDevice {
            device,
            config,
            name,
            resampled_to: (!shares_clock).then_some(output.config.sample_rate),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn channels(&self) -> u16 {
        self.config.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Whether the input runs on a clock of its own, and is resampled to
    /// the output's.
    pub fn is_resampled(&self) -> bool {
        self.resampled_to.is_some()
    }

    fn start(self) -> Result<(Stream, CushionReader, Arc<Counters>)> {
        let channels = usize::from(self.config.channels);
        let (mut writer, reader, counters) = match self.resampled_to {
            None => {
                let (writer, reader, counters) = cushion::cushion(channels, CUSHION_FRAMES);
                (InputWriter::Direct(writer), reader, counters)
            }
            Some(output_rate) => {
                let (writer, reader, counters) =
                    cushion::steered_cushion(channels, CUSHION_FRAMES, output_rate);
                let resampler = InputResampler::new(writer, self.config.sample_rate, output_rate)?;
                (
                    InputWriter::Resampled(Box::new(resampler)),
                    reader,
                    counters,
                )
            }
        };
        let stream = self
            .device
            .build_input_stream(
                self.config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    writer.write(data, Instant::now());
                },
                |error| eprintln!("audio input: {error}"),
                None,
            )
            .map_err(|e| Error::context(&format!("opening \"{}\"", self.name), e))?;
        stream
            .play()
            .map_err(|e| Error::context(&format!("starting \"{}\"", self.name), e))?;
        Ok((stream, reader, counters))
    }
}

/// What the input callback writes into the cushion through.
enum InputWriter {
    Direct(CushionWriter),
    Resampled(Box<InputResampler>),
}

impl InputWriter {
    fn write(&mut self, samples: &[f32], at: Instant) {
        match self {
            InputWriter::Direct(writer) => writer.write(samples, at),
            InputWriter::Resampled(resampler) => resampler.write(samples, at),
        }
    }
}

fn same_device(a: &Device, b: &Device) -> bool {
    match (a.id(), b.id()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.to_string() == b.to_string(),
    }
}

/// Running audio streams. Audio stops when this is dropped.
pub struct AudioOutput {
    _output: Stream,
    _input: Option<Stream>,
    counters: Option<Arc<Counters>>,
}

impl AudioOutput {
    /// How the input has fared, if one was opened.
    pub fn input_stats(&self) -> Option<InputStats> {
        self.counters.as_ref().map(|counters| counters.stats())
    }
}
