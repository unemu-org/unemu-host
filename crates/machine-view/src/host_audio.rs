// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host playback of a machine's drained sound; no hardware is modelled here.
//!
//! See `backlog/docs/architecture.md` § Core boundary and host timing. Only the window explicitly
//! opens a device. Playback consumes completed virtual-time frames and cannot
//! change the machine, its clock or host-clock pacing policy. Audio
//! observations feed a 200 ms stereo ring at run boundaries; callbacks use
//! silence on underrun and drop oldest frames on overrun. Playback prefills
//! 50 ms and trims above 70 ms; recoverable host/source errors leave the
//! window running.
//!
//! A machine drains its sound at the exact rate its description states, any
//! rate from 8 000 to 192 000 frames a second ([`SOURCE_RATES`];
//! core-contract.md §13.2, "Sound at any rate"). The device plays whatever
//! rate the host offers, and the producer converts between the two by linear
//! interpolation at an exact rational position. A 48 kHz source, the PC's,
//! takes the arithmetic it took when it was the only rate.
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SampleFormat, SizedSample, SupportedStreamConfig, SupportedStreamConfigRange,
};
use unemu_core_api::Rate;
/// One stereo frame of signed 16-bit samples, left then right.
pub type StereoFrame = [i16; 2];

/// The device rate the session asks the host for first, in stereo frames per
/// second, and the source rate of a [`HostAudio::new`] (the PC mixes at
/// 48 kHz).
pub const FRAMES_PER_SECOND: u32 = 48_000;

/// The source rates the window plays, in whole frames per second, inclusive:
/// the bounds the contract sets on `AudioDescription::rate`.
pub const SOURCE_RATES: std::ops::RangeInclusive<u64> = 8_000..=192_000;

/// Whether the window plays sound drained at `rate`: an exact rate within
/// [`SOURCE_RATES`].
pub fn plays(rate: Rate) -> bool {
    let (numerator, denominator) = (u128::from(rate.numerator), u128::from(rate.denominator));
    denominator != 0
        && numerator >= u128::from(*SOURCE_RATES.start()) * denominator
        && numerator <= u128::from(*SOURCE_RATES.end()) * denominator
}

/// A rate for people: `44100 Hz`, or `984375/22 Hz` when it is not whole.
fn hertz(rate: Rate) -> String {
    if rate.denominator == 1 {
        format!("{} Hz", rate.numerator)
    } else {
        format!("{}/{} Hz", rate.numerator, rate.denominator)
    }
}

use std::{
    error::Error,
    io,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
};

/// Linear interpolation carries an exact rational position across input blocks.
/// Only the producer uses this state; see backlog/docs/architecture.md, Core boundary and host timing for quality limits.
///
/// With the source at `n/d` frames a second and the device at `r`, one input
/// interval is `r·d` units of position and one output interval is `n`. Per
/// input frame the converter emits while the position is at most `r·d`,
/// adding `n` per output, then takes `r·d` off. For a 48 000/1 source that
/// is the arithmetic of the converter that knew only 48 kHz. Wide integers
/// keep any denominator a description may state exact.
struct RateConverter {
    source: Rate,
    rate: u32,
    previous: Option<StereoFrame>,
    phase: u128,
}

impl RateConverter {
    /// A converter from `source`, which [`plays`] accepts, to a device at
    /// `rate`.
    fn new(source: Rate, rate: u32) -> Self {
        Self {
            source,
            rate,
            previous: None,
            phase: 0,
        }
    }

    /// Whether the device plays the source's rate, so frames need no
    /// conversion.
    fn passes_through(&self) -> bool {
        self.source.denominator == 1 && self.source.numerator == u64::from(self.rate)
    }

    fn process(&mut self, frames: &[StereoFrame], mut emit: impl FnMut(StereoFrame)) {
        let span = u128::from(self.rate) * u128::from(self.source.denominator);
        let step = u128::from(self.source.numerator);
        for &current in frames {
            if let Some(previous) = self.previous {
                while self.phase <= span {
                    let frame = std::array::from_fn(|channel| {
                        let left = i128::from(previous[channel]);
                        let right = i128::from(current[channel]);
                        (left + (right - left) * self.phase as i128 / span as i128) as i16
                    });
                    emit(frame);
                    self.phase += step;
                }
                self.phase -= span;
            } else {
                emit(current);
                self.phase = step;
            }
            self.previous = Some(current);
        }
    }
}

struct Ring {
    frames: Box<[StereoFrame]>,
    head: usize,
    len: usize,
    primed: bool,
    target: usize,
    trim_threshold: usize,
}

impl Ring {
    fn new(capacity: usize, rate: u32) -> Self {
        Self {
            frames: vec![[0; 2]; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
            primed: false,
            target: (rate as usize / 20).min(capacity),
            trim_threshold: (rate as usize * 70 / 1000).min(capacity),
        }
    }

    fn discard(&mut self, count: usize) {
        self.head = (self.head + count) % self.frames.len();
        self.len -= count;
    }

    fn push(&mut self, frames: &[StereoFrame]) {
        let capacity = self.frames.len();
        let frames = &frames[frames.len().saturating_sub(capacity)..];
        self.discard((self.len + frames.len()).saturating_sub(capacity));
        let tail = (self.head + self.len) % capacity;
        let first = frames.len().min(capacity - tail);
        self.frames[tail..tail + first].copy_from_slice(&frames[..first]);
        self.frames[..frames.len() - first].copy_from_slice(&frames[first..]);
        self.len += frames.len();
        if self.len > self.trim_threshold {
            self.discard(self.len - self.target);
        }
    }

    fn read(&mut self, output: &mut [StereoFrame]) -> usize {
        if !self.primed && self.len < self.target {
            return 0;
        }
        self.primed = true;
        let count = output.len().min(self.len);
        let first = count.min(self.frames.len() - self.head);
        output[..first].copy_from_slice(&self.frames[self.head..self.head + first]);
        output[first..count].copy_from_slice(&self.frames[..count - first]);
        self.discard(count);
        if count < output.len() {
            self.primed = false;
        }
        count
    }
}

/// These notifications leave the cpal stream usable; other errors are fatal.
fn transient_error_bit(kind: cpal::ErrorKind) -> Option<u8> {
    match kind {
        cpal::ErrorKind::Xrun => Some(1),
        cpal::ErrorKind::DeviceChanged => Some(2),
        cpal::ErrorKind::RealtimeDenied => Some(4),
        _ => None,
    }
}

fn record_stream_error(kind: cpal::ErrorKind, active: &AtomicBool, notices: &AtomicU8) {
    if let Some(bit) = transient_error_bit(kind) {
        notices.fetch_or(bit, Ordering::Relaxed);
    } else {
        active.store(false, Ordering::Relaxed);
    }
}

/// The producer half of the ring, shared with the host's output callback.
#[derive(Clone)]
pub struct WindowAudioSink {
    ring: Arc<Mutex<Ring>>,
    converter: Arc<Mutex<RateConverter>>,
    active: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}

impl WindowAudioSink {
    fn new(capacity: usize, source: Rate, rate: u32) -> Self {
        Self {
            ring: Arc::new(Mutex::new(Ring::new(capacity, rate))),
            converter: Arc::new(Mutex::new(RateConverter::new(source, rate))),
            active: Arc::new(AtomicBool::new(false)),
            muted: Arc::new(AtomicBool::new(false)),
        }
    }

    fn configure(&self, rate: u32) {
        if let Ok(mut converter) = self.converter.lock() {
            *converter = RateConverter::new(converter.source, rate);
            if let Ok(mut ring) = self.ring.lock() {
                *ring = Ring::new(rate as usize / 5, rate);
            }
        }
    }

    fn read<T: SizedSample + FromSample<i16>>(&self, output: &mut [T], channels: usize) {
        output.fill(T::from_sample(0));
        if !self.enabled() {
            return;
        }
        let mut scratch = [[0; 2]; 1024];
        for chunk in output.chunks_mut(channels * scratch.len()) {
            let count = chunk.len() / channels;
            // Only two bounded PCM copies hold the mutex. Conversion and extra
            // host channels must not lengthen the producer's critical section.
            let copied = match self.ring.lock() {
                Ok(mut ring) => ring.read(&mut scratch[..count]),
                Err(_) => return,
            };
            for (frame, samples) in chunk.chunks_exact_mut(channels).zip(&scratch[..copied]) {
                frame[0] = T::from_sample(samples[0]);
                frame[1] = T::from_sample(samples[1]);
            }
            if copied < count {
                break;
            }
        }
    }

    /// Queue drained frames for playback; nothing while playback is off.
    pub fn push(&self, frames: &[StereoFrame]) {
        if !self.enabled() {
            return;
        }
        if let Ok(mut converter) = self.converter.lock() {
            if converter.passes_through() {
                if let Ok(mut ring) = self.ring.lock() {
                    ring.push(frames);
                }
                return;
            }
            let mut scratch = [[0; 2]; 1024];
            let mut len = 0;
            converter.process(frames, |frame| {
                scratch[len] = frame;
                len += 1;
                if len == scratch.len() {
                    if let Ok(mut ring) = self.ring.lock() {
                        ring.push(&scratch);
                    }
                    len = 0;
                }
            });
            if let Ok(mut ring) = self.ring.lock() {
                ring.push(&scratch[..len]);
            }
        }
    }
}

impl WindowAudioSink {
    /// Whether frames are wanted: a stream plays and no mute holds it back.
    pub fn enabled(&self) -> bool {
        self.active.load(Ordering::Relaxed) && !self.muted.load(Ordering::Relaxed)
    }

    /// Empty the ring and the converter's history, then leave the sink
    /// `muted` or not.
    fn restart(&self, muted: bool) {
        // Keep callbacks silent until old samples and interpolation history are gone.
        self.muted.store(true, Ordering::Relaxed);
        if let Ok(mut converter) = self.converter.lock() {
            *converter = RateConverter::new(converter.source, converter.rate);
            if let Ok(mut ring) = self.ring.lock() {
                ring.len = 0;
                ring.primed = false;
            }
        }
        self.muted.store(muted, Ordering::Relaxed);
    }
}

/// The device configuration to open: stereo or more, i16 before f32, at
/// `preferred` frames a second when a range offers it, else the host's
/// default, else the supported rate nearest `preferred`.
fn select_config(
    ranges: &[SupportedStreamConfigRange],
    default: Option<&SupportedStreamConfig>,
    preferred: u32,
) -> Option<SupportedStreamConfig> {
    let supported = |channels, rate, format| {
        channels >= 2 && rate > 0 && matches!(format, SampleFormat::I16 | SampleFormat::F32)
    };
    [SampleFormat::I16, SampleFormat::F32]
        .into_iter()
        .find_map(|format| {
            ranges.iter().find_map(|range| {
                (range.channels() >= 2 && range.sample_format() == format)
                    .then(|| range.try_with_sample_rate(preferred))
                    .flatten()
            })
        })
        .or_else(|| {
            default
                .filter(|config| {
                    supported(
                        config.channels(),
                        config.sample_rate(),
                        config.sample_format(),
                    )
                })
                .cloned()
        })
        .or_else(|| {
            ranges
                .iter()
                .filter(|range| {
                    supported(
                        range.channels(),
                        range.min_sample_rate(),
                        range.sample_format(),
                    )
                })
                .map(|range| {
                    range.with_sample_rate(
                        preferred.clamp(range.min_sample_rate(), range.max_sample_rate()),
                    )
                })
                .min_by_key(|config| {
                    (
                        config.sample_rate().abs_diff(preferred),
                        u8::from(config.sample_format() != SampleFormat::I16),
                        config.channels(),
                        std::cmp::Reverse(config.sample_rate()),
                    )
                })
        })
}

/// Host playback: the ring, the output stream and both mutes.
pub struct HostAudio {
    sink: WindowAudioSink,
    stream: Option<cpal::Stream>,
    attempted: bool,
    reported: bool,
    notices: Arc<AtomicU8>,
    reported_notices: u8,
    started: bool,
    user_muted: bool,
    speed_muted: bool,
    /// Why no host device may be opened, if one may not.
    device_refused: Option<String>,
    /// The rate the machine drains its sound at, as described, even when
    /// the window does not play it.
    source: Rate,
}

impl Default for HostAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl HostAudio {
    /// Playback of sound drained at [`FRAMES_PER_SECOND`].
    pub fn new() -> Self {
        Self::playing(Rate::new(u64::from(FRAMES_PER_SECOND), 1))
    }

    /// Playback of sound drained at `source`, a machine's described rate. A
    /// rate the window does not play ([`plays`]) refuses the device, so
    /// [`Self::open`] reports it once and the machine is never asked for
    /// sound.
    pub fn for_source(source: Rate) -> Self {
        if plays(source) {
            return Self::playing(source);
        }
        let mut audio = Self::new();
        audio.source = source;
        audio.refuse_device(format!(
            "the machine's sound runs at {}; the window plays {} to {} Hz",
            hertz(source),
            SOURCE_RATES.start(),
            SOURCE_RATES.end()
        ));
        audio
    }

    fn playing(source: Rate) -> Self {
        Self {
            sink: WindowAudioSink::new(FRAMES_PER_SECOND as usize / 5, source, FRAMES_PER_SECOND),
            stream: None,
            attempted: false,
            reported: false,
            notices: Arc::new(AtomicU8::new(0)),
            reported_notices: 0,
            started: false,
            user_muted: false,
            speed_muted: false,
            device_refused: None,
            source,
        }
    }

    /// Never open a host device; [`Self::open`] reports `reason` instead.
    /// A shell's tests refuse the device this way, and a session refuses it
    /// for sound at a rate it does not play.
    pub fn refuse_device(&mut self, reason: impl Into<String>) {
        self.device_refused = Some(reason.into());
    }

    /// The rate the sound arrives at, in frames a second, as the machine
    /// describes it; also a rate the window refused.
    pub fn source(&self) -> Rate {
        self.source
    }

    pub fn sink(&self) -> WindowAudioSink {
        self.sink.clone()
    }

    /// Queue a machine's drained frames for playback.
    pub fn push(&self, frames: &[StereoFrame]) {
        self.sink.push(frames);
    }

    /// Whether the session should ask the machine for sound now: a stream
    /// plays and no mute holds it back.
    pub fn enabled(&self) -> bool {
        self.sink.enabled()
    }

    /// Behave as if a host stream played, without opening one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn activate_for_test(&self) {
        self.sink.active.store(true, Ordering::Relaxed);
    }

    /// Everything the host callback would play next, ignoring the prefill wait.
    /// Behave as if a host stream started or failed, without opening one.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_active_for_test(&self, active: bool) {
        self.sink.active.store(active, Ordering::Relaxed);
    }

    /// Size the ring and the converter for a device at `rate`, as opening
    /// one would.
    #[cfg(any(test, feature = "test-support"))]
    pub fn configure_for_test(&self, rate: u32) {
        self.sink.configure(rate);
    }

    /// Whether opening a device was attempted, and whether a stream exists.
    #[cfg(any(test, feature = "test-support"))]
    pub fn opened_for_test(&self) -> (bool, bool) {
        (self.attempted, self.stream.is_some())
    }

    /// Frames waiting in the ring.
    #[cfg(any(test, feature = "test-support"))]
    pub fn queued_for_test(&self) -> usize {
        self.sink.ring.lock().map_or(0, |ring| ring.len)
    }

    /// What the host callback would write into `output` now.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read_for_test<T: SizedSample + FromSample<i16>>(
        &self,
        output: &mut [T],
        channels: usize,
    ) {
        self.sink.read(output, channels);
    }

    /// Everything the host callback would play next, ignoring the prefill wait.
    #[cfg(any(test, feature = "test-support"))]
    pub fn drain_for_test(&self) -> Vec<StereoFrame> {
        let mut ring = self.sink.ring.lock().unwrap();
        let mut frames = vec![[0; 2]; ring.len];
        ring.primed = true;
        let count = ring.read(&mut frames);
        frames.truncate(count);
        frames
    }

    pub fn available(&self) -> bool {
        self.sink.active.load(Ordering::Relaxed)
    }

    pub fn muted(&self) -> bool {
        self.sink.muted.load(Ordering::Relaxed)
    }

    /// Whether the guest's sound reaches the speakers: a device is active and
    /// neither mute holds it back.
    pub fn audible(&self) -> bool {
        self.available() && !self.speed_muted && !self.user_muted
    }

    pub fn status_text(&self) -> &'static str {
        if !self.available() {
            "Audio unavailable"
        } else if self.speed_muted && self.user_muted {
            "Audio muted (speed + user)"
        } else if self.speed_muted {
            "Audio muted (speed)"
        } else if self.user_muted {
            "Audio muted (user)"
        } else {
            "Audio on"
        }
    }

    pub fn mute_label(&self) -> &'static str {
        if self.speed_muted && self.user_muted {
            "User mute: on (speed muted)"
        } else if self.speed_muted {
            "User mute: off (speed muted)"
        } else if self.user_muted {
            "Unmute audio"
        } else {
            "Mute audio"
        }
    }

    /// Whether the user muted the sound, whatever the speed does.
    pub fn user_muted(&self) -> bool {
        self.user_muted
    }

    pub fn toggle_mute(&mut self) {
        if !self.available() {
            return;
        }
        self.user_muted = !self.user_muted;
        self.update_mute();
    }

    pub fn set_speed_muted(&mut self, muted: bool) {
        self.speed_muted = muted;
        self.update_mute();
    }

    fn update_mute(&mut self) {
        let muted = self.user_muted || self.speed_muted;
        if self.muted() == muted {
            return;
        }
        self.sink.restart(muted);
    }

    /// Drop the sound queued and not yet played, with the converter's
    /// history, so what is pushed next plays as a new stream. After a saved
    /// state is loaded the machine's sound jumps in time; what it drained
    /// before the load is not played after it.
    pub fn discard_pending(&self) {
        self.sink.restart(self.muted());
    }

    pub fn open(&mut self, warnings: &mut impl io::Write) {
        if self.attempted {
            return;
        }
        self.attempted = true;
        if let Err(error) = self.start(warnings) {
            self.sink.active.store(false, Ordering::Relaxed);
            let _ = writeln!(warnings, "audio disabled: {error}");
            self.reported = true;
        }
    }

    fn start(&mut self, warnings: &mut impl io::Write) -> Result<(), Box<dyn Error>> {
        if let Some(reason) = &self.device_refused {
            return Err(reason.clone().into());
        }
        // A test plays nothing through the host's speakers; it behaves as a
        // host without an output device does.
        if cfg!(test) {
            return Err("host audio output is not opened in tests".into());
        }
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default output device")?;
        let ranges: Vec<_> = device.supported_output_configs()?.collect();
        let default = device.default_output_config().ok();
        let source = self.source();
        // A device at the source's own rate plays it unconverted; a rate that
        // is not whole is converted from whatever the host offers first.
        let preferred = match (source.denominator, u32::try_from(source.numerator)) {
            (1, Ok(whole)) => whole,
            _ => FRAMES_PER_SECOND,
        };
        let config = select_config(&ranges, default.as_ref(), preferred)
            .ok_or("output with at least two channels (i16/f32) is unavailable")?;
        let rate = config.sample_rate();
        self.sink.configure(rate);
        self.sink.active.store(true, Ordering::Relaxed);
        let stream = match config.sample_format() {
            SampleFormat::I16 => self.build_stream::<i16>(&device, config)?,
            _ => self.build_stream::<f32>(&device, config)?,
        };
        self.stream = Some(stream);
        if source != Rate::new(u64::from(rate), 1) {
            let name = device
                .description()
                .map(|description| description.name().to_owned())
                .unwrap_or_else(|_| "default output device".to_owned());
            let _ = writeln!(
                warnings,
                "audio: converting {} to {rate} Hz for {name}",
                hertz(source)
            );
        }
        Ok(())
    }

    fn build_stream<T: SizedSample + FromSample<i16>>(
        &self,
        device: &cpal::Device,
        config: SupportedStreamConfig,
    ) -> Result<cpal::Stream, cpal::Error> {
        let sink = self.sink.clone();
        let active = self.sink.active.clone();
        let notices = self.notices.clone();
        let channels = usize::from(config.channels());
        device.build_output_stream(
            config.config(),
            move |output: &mut [T], _| sink.read(output, channels),
            // Reporting and dropping the failed stream belong to the event loop,
            // so the callback neither allocates nor writes to stderr.
            move |error| record_stream_error(error.kind(), &active, &notices),
            None,
        )
    }

    pub fn check_failure(&mut self, warnings: &mut impl io::Write) -> bool {
        let notices = self.notices.load(Ordering::Relaxed);
        for (bit, message) in [
            (1, "underrun or overrun"),
            (2, "device rerouted"),
            (4, "realtime scheduling denied"),
        ] {
            if notices & bit != 0 && self.reported_notices & bit == 0 {
                let _ = writeln!(warnings, "audio warning: {message}");
                self.reported_notices |= bit;
            }
        }
        if self.available() && !self.started {
            let ready = self
                .sink
                .ring
                .lock()
                .is_ok_and(|ring| ring.len >= ring.target);
            if ready {
                if let Some(stream) = &self.stream {
                    if stream.play().is_err() {
                        self.sink.active.store(false, Ordering::Relaxed);
                    } else {
                        self.started = true;
                    }
                }
            }
        }
        if self.attempted && !self.reported && !self.available() {
            self.stream = None;
            let _ = writeln!(warnings, "audio disabled: output stream failed");
            self.reported = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests;
