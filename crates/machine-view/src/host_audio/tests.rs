// SPDX-License-Identifier: AGPL-3.0-or-later
//! Playback tests use only memory; they never open a host device.

use super::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

/// The PC's rate, the one source rate the converter knew before.
const PC_RATE: Rate = Rate::new(48_000, 1);
const CAPACITY: usize = FRAMES_PER_SECOND as usize / 5;
const TARGET: usize = FRAMES_PER_SECOND as usize / 20;
const MARGIN: usize = FRAMES_PER_SECOND as usize / 50;

struct CountingAllocator;
thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
// Counting is opt-in and thread-local; all other tests still allocate through
// System without contributing to this test's counter.
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn active_sink(capacity: usize) -> WindowAudioSink {
    let sink = WindowAudioSink::new(capacity, PC_RATE, FRAMES_PER_SECOND);
    sink.active.store(true, Ordering::Relaxed);
    sink.ring.lock().unwrap().primed = true;
    sink
}

#[test]
fn underrun_fills_every_sample_with_silence() {
    let sink = active_sink(3);
    sink.push(&[[12, -34]]);
    let mut output = [99i16; 6];
    sink.read(&mut output, 2);
    assert_eq!(output, [12, -34, 0, 0, 0, 0]);
}

#[test]
fn overrun_drops_oldest_complete_frames_in_order() {
    let sink = active_sink(3);
    sink.push(&[[1, -1], [2, -2]]);
    sink.push(&[[3, -3], [4, -4], [5, -5], [6, -6]]);
    let mut output = [0i16; 6];
    sink.read(&mut output, 2);
    assert_eq!(output, [4, -4, 5, -5, 6, -6]);
}

#[test]
fn wraparound_preserves_exact_stereo_order() {
    let sink = active_sink(3);
    sink.push(&[[1, 2], [3, 4]]);
    let mut first = [0i16; 2];
    sink.read(&mut first, 2);
    assert_eq!(first, [1, 2]);
    sink.push(&[[5, 6], [7, 8]]);
    let mut output = [0i16; 6];
    sink.read(&mut output, 2);
    assert_eq!(output, [3, 4, 5, 6, 7, 8]);
}

#[test]
fn callback_does_not_allocate_for_data_or_silence() {
    let sink = active_sink(3);
    sink.push(&[[1, 2], [3, 4]]);
    let mut integer = [0i16; 8];
    let mut float = [0.0f32; 8];
    ALLOCATIONS.with(|n| n.set(Some(0)));
    sink.read(&mut integer, 2);
    sink.push(&[[16384, -16384]; 3]);
    sink.read(&mut float, 2);
    sink.read(&mut integer, 2);
    let count = ALLOCATIONS.with(|n| n.replace(None));
    assert_eq!(count, Some(0));
    assert_eq!(integer, [0i16; 8]);
    assert_eq!(float, [0.5, -0.5, 0.5, -0.5, 0.5, -0.5, 0.0, 0.0]);
}

#[test]
fn sink_adapter_preserves_i16_and_normalizes_f32() {
    let frames = [[i16::MIN, i16::MAX], [-16384, 16384]];
    let sink = active_sink(3);
    sink.push(&frames);
    let mut integer = [0i16; 4];
    sink.read(&mut integer, 2);
    assert_eq!(integer, [-32768, 32767, -16384, 16384]);
    sink.push(&frames);
    let mut float = [0.0f32; 4];
    sink.read(&mut float, 2);
    assert_eq!(float, [-1.0, 32767.0 / 32768.0, -0.5, 0.5]);
}

fn range(channels: u16, rate: u32, format: SampleFormat) -> SupportedStreamConfigRange {
    SupportedStreamConfigRange::new(
        channels,
        rate,
        rate,
        cpal::SupportedBufferSize::Unknown,
        format,
    )
}

#[test]
fn config_prefers_i16_then_f32_at_48_khz_with_stereo_or_more() {
    let float = range(2, 48000, SampleFormat::F32);
    let integer = range(2, 48000, SampleFormat::I16);
    assert_eq!(
        select_config(&[float, integer], None, FRAMES_PER_SECOND)
            .unwrap()
            .sample_format(),
        SampleFormat::I16
    );
    assert_eq!(
        select_config(&[float], None, FRAMES_PER_SECOND)
            .unwrap()
            .sample_format(),
        SampleFormat::F32
    );
    assert!(select_config(
        &[
            range(2, 48000, SampleFormat::U16),
            range(1, 48000, SampleFormat::F32)
        ],
        None,
        FRAMES_PER_SECOND,
    )
    .is_none());
}

#[test]
fn construction_is_inert_without_a_device_and_mute_discards_pending_audio() {
    let mut host = HostAudio::new();
    assert!(!host.attempted);
    assert!(host.stream.is_none());
    assert!(!host.sink.enabled());
    host.sink.push(&[[1, 2]]);
    assert_eq!(host.sink.ring.lock().unwrap().len, 0);
    host.sink.active.store(true, Ordering::Relaxed);
    host.sink.push(&[[3, 4]]);
    host.toggle_mute();
    assert!(!host.sink.enabled());
    host.toggle_mute();
    let mut output = [99i16; 2];
    host.sink.read(&mut output, 2);
    assert_eq!(output, [0, 0]);
}

/// Opening host audio, as the shell does when a machine starts, never
/// reaches the developer's speakers from a test: the sink stays disabled as
/// on a host without an output device, and the reason is reported once.
#[test]
fn opening_in_a_test_never_starts_an_output_stream() {
    let mut host = HostAudio::new();
    let mut warnings = Vec::new();
    host.open(&mut warnings);
    host.open(&mut warnings);
    assert!(host.attempted);
    assert!(host.stream.is_none());
    assert!(!host.available());
    assert_eq!(
        String::from_utf8(warnings).unwrap(),
        "audio disabled: host audio output is not opened in tests\n"
    );
}

#[test]
fn stream_failure_disables_sink_and_reports_once_on_host_thread() {
    let mut host = HostAudio::new();
    host.attempted = true;
    host.sink.active.store(true, Ordering::Relaxed);
    host.sink.push(&[[1, 2]]);
    // The real error callback performs only this atomic store.
    host.sink.active.store(false, Ordering::Relaxed);
    let mut warnings = Vec::new();
    host.check_failure(&mut warnings);
    host.check_failure(&mut warnings);
    assert_eq!(warnings, b"audio disabled: output stream failed\n");
    assert!(!host.sink.enabled());
    let mut output = [99i16; 2];
    host.sink.read(&mut output, 2);
    assert_eq!(output, [0, 0]);
}

#[test]
fn transient_cpal_errors_keep_stream_active_and_log_each_kind_once() {
    use cpal::ErrorKind::*;
    let mut host = HostAudio::new();
    host.attempted = true;
    host.sink.active.store(true, Ordering::Relaxed);
    let mut warnings = Vec::new();
    for _ in 0..2 {
        for kind in [Xrun, DeviceChanged, RealtimeDenied] {
            assert!(transient_error_bit(kind).is_some());
            record_stream_error(kind, &host.sink.active, &host.notices);
            host.check_failure(&mut warnings);
            assert!(host.available());
        }
    }
    assert_eq!(String::from_utf8(warnings).unwrap().lines().count(), 3);
    for kind in [
        DeviceBusy,
        DeviceNotAvailable,
        HostUnavailable,
        InvalidInput,
        PermissionDenied,
        ResourceExhausted,
        StreamInvalidated,
        UnsupportedConfig,
        UnsupportedOperation,
        BackendError,
        Other,
    ] {
        assert_eq!(transient_error_bit(kind), None);
        host.sink.active.store(true, Ordering::Relaxed);
        record_stream_error(kind, &host.sink.active, &host.notices);
        assert!(!host.available());
    }
}

#[test]
fn prefill_and_latency_trim_keep_recent_audio_near_fifty_ms() {
    let sink = WindowAudioSink::new(CAPACITY, PC_RATE, FRAMES_PER_SECOND);
    sink.active.store(true, Ordering::Relaxed);
    let mut output = [99i16; 2];
    sink.push(&vec![[1, -1]; TARGET - 1]);
    sink.read(&mut output, 2);
    assert_eq!(output, [0, 0]);
    assert_eq!(sink.ring.lock().unwrap().len, TARGET - 1);
    sink.push(&[[2, -2]]);
    sink.read(&mut output, 2);
    assert_eq!(output, [1, -1]);
    sink.push(&vec![[3, -3]; TARGET + MARGIN + 1]);
    assert_eq!(sink.ring.lock().unwrap().len, TARGET);
    sink.read(&mut output, 2);
    assert_eq!(output, [3, -3]);
    let mut drain = vec![0i16; TARGET * 2];
    sink.read(&mut drain, 2);
    sink.push(&[[4, -4]]);
    sink.read(&mut output, 2);
    assert_eq!(output, [0, 0], "an underrun rearms prefill");
}

#[test]
fn multichannel_devices_keep_stereo_first_and_zero_extra_channels() {
    let config = select_config(
        &[range(6, 48000, SampleFormat::F32)],
        None,
        FRAMES_PER_SECOND,
    )
    .unwrap();
    assert_eq!(config.channels(), 6);
    let sink = active_sink(3);
    sink.push(&[[i16::MIN, 16384], [0, i16::MAX]]);
    let mut output = [99.0f32; 18];
    sink.read(&mut output, 6);
    assert_eq!(&output[..6], &[-1.0, 0.5, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(
        &output[6..12],
        &[0.0, 32767.0 / 32768.0, 0.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(&output[12..], &[0.0; 6]);
}

#[test]
fn config_uses_default_after_preferring_48_khz() {
    let default = range(2, 16000, SampleFormat::F32).with_sample_rate(16000);
    let ranges = [
        range(2, 44100, SampleFormat::I16),
        range(2, 16000, SampleFormat::F32),
    ];
    let selected = select_config(&ranges, Some(&default), FRAMES_PER_SECOND).unwrap();
    assert_eq!(selected.sample_rate(), 16000);
    assert_eq!(selected.sample_format(), SampleFormat::F32);
    let selected = select_config(
        &[range(2, 48000, SampleFormat::F32)],
        Some(&default),
        FRAMES_PER_SECOND,
    )
    .unwrap();
    assert_eq!(selected.sample_rate(), 48000);
}

#[test]
fn config_falls_back_to_nearest_rate_then_format_channels_and_higher_rate() {
    let ranges = [
        range(2, 16000, SampleFormat::I16),
        range(2, 44100, SampleFormat::F32),
        range(6, 44100, SampleFormat::I16),
        range(2, 44100, SampleFormat::I16),
    ];
    for default in [
        None,
        Some(range(1, 32000, SampleFormat::I16).with_sample_rate(32000)),
        Some(range(2, 32000, SampleFormat::U16).with_sample_rate(32000)),
    ] {
        let selected = select_config(&ranges, default.as_ref(), FRAMES_PER_SECOND).unwrap();
        assert_eq!(selected.sample_rate(), 44100);
        assert_eq!(selected.sample_format(), SampleFormat::I16);
        assert_eq!(selected.channels(), 2);
    }
    let continuous = SupportedStreamConfigRange::new(
        2,
        22050,
        44100,
        cpal::SupportedBufferSize::Unknown,
        SampleFormat::F32,
    );
    assert_eq!(
        select_config(&[continuous], None, FRAMES_PER_SECOND)
            .unwrap()
            .sample_rate(),
        44100
    );
    assert_eq!(
        select_config(
            &[
                range(2, 32000, SampleFormat::I16),
                range(2, 64000, SampleFormat::I16),
            ],
            None,
            FRAMES_PER_SECOND,
        )
        .unwrap()
        .sample_rate(),
        64000
    );
    assert!(select_config(&[], None, FRAMES_PER_SECOND).is_none());
    assert!(select_config(
        &[range(1, 44100, SampleFormat::F32)],
        None,
        FRAMES_PER_SECOND
    )
    .is_none());
}

fn tone(frames: usize) -> Vec<StereoFrame> {
    (0..frames)
        .map(|index| {
            let phase = std::f64::consts::TAU * 1000.0 * index as f64 / 48000.0;
            [
                (12000.0 * phase.sin()).round() as i16,
                (9000.0 * phase.cos()).round() as i16,
            ]
        })
        .collect()
}

fn irregular_blocks(input: &[StereoFrame], mut consume: impl FnMut(&[StereoFrame])) {
    let mut remaining = input;
    let mut seed = 7u32;
    consume(&[]);
    while !remaining.is_empty() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let count = (1 + seed as usize % 2048).min(remaining.len());
        consume(&remaining[..count]);
        remaining = &remaining[count..];
    }
}

fn check_tone(rate: u32) {
    let input = tone(48000 * 10);
    let mut converter = RateConverter::new(PC_RATE, rate);
    let mut output = Vec::new();
    let mut consumed = 0u64;
    irregular_blocks(&input, |block| {
        converter.process(block, |frame| output.push(frame));
        consumed += block.len() as u64;
        let numerator = consumed * u64::from(rate);
        assert!((numerator / 48000..=numerator.div_ceil(48000)).contains(&(output.len() as u64)));
    });
    assert_eq!(output.len(), rate as usize * 10);
    for (index, frame) in output.iter().enumerate() {
        let phase = std::f64::consts::TAU * 1000.0 * index as f64 / f64::from(rate);
        for (sample, expected) in frame
            .iter()
            .zip([12000.0 * phase.sin(), 9000.0 * phase.cos()])
        {
            assert!((f64::from(*sample) - expected).abs() <= 28.0);
        }
    }
}

#[test]
fn tone_at_44100_has_exact_duration_and_preserves_stereo_frequency() {
    check_tone(44100);
}

#[test]
fn tone_at_16000_has_exact_duration_and_preserves_stereo_frequency() {
    check_tone(16000);
}

#[test]
fn conversion_preserves_constants_and_extreme_samples() {
    for rate in [16000, 44100, 96000] {
        for frame in [[12345, -23456], [i16::MIN, i16::MAX], [0, 0]] {
            let mut converter = RateConverter::new(PC_RATE, rate);
            let mut output = Vec::new();
            irregular_blocks(&vec![frame; 48000], |block| {
                converter.process(block, |frame| output.push(frame));
            });
            assert!(!output.is_empty());
            assert!(output.iter().all(|value| *value == frame));
        }
    }
}

#[test]
fn conversion_carries_fractional_phase_across_blocks() {
    let input = tone(48000);
    for rate in [16000, 44100, 96000] {
        let mut whole = Vec::new();
        RateConverter::new(PC_RATE, rate).process(&input, |frame| whole.push(frame));
        let mut split = Vec::new();
        let mut converter = RateConverter::new(PC_RATE, rate);
        irregular_blocks(&input, |block| {
            converter.process(block, |frame| split.push(frame))
        });
        assert_eq!(whole, split);
        let mut single_frames = Vec::new();
        let mut converter = RateConverter::new(PC_RATE, rate);
        for frame in &input {
            converter.process(std::slice::from_ref(frame), |frame| {
                single_frames.push(frame)
            });
        }
        assert_eq!(whole, single_frames);
    }
}

#[test]
fn producer_converts_before_ring_write_without_allocations() {
    for rate in [16000, 44100] {
        let sink = active_sink(3);
        let subscribed = sink.clone();
        sink.configure(rate);
        let input = tone(480);
        let mut expected = Vec::new();
        RateConverter::new(PC_RATE, rate).process(&input, |frame| expected.push(frame));
        ALLOCATIONS.with(|n| n.set(Some(0)));
        irregular_blocks(&input, |block| subscribed.push(block));
        let count = ALLOCATIONS.with(|n| n.replace(None));
        assert_eq!(count, Some(0));
        let mut ring = sink.ring.lock().unwrap();
        assert_eq!(ring.len, rate as usize / 100);
        assert_eq!(ring.len, expected.len());
        ring.primed = true;
        let mut actual = vec![[0; 2]; expected.len()];
        ring.read(&mut actual);
        assert_eq!(actual, expected);
    }
}

#[test]
fn ring_capacity_prefill_and_trim_use_device_rate_frames() {
    for rate in [16000, 44100] {
        let mut ring = Ring::new(rate as usize / 5, rate);
        let target = rate as usize / 20;
        let threshold = rate as usize * 70 / 1000;
        assert_eq!(ring.frames.len(), rate as usize * 200 / 1000);
        let mut output = [[0; 2]; 1];
        ring.push(&vec![[1, -1]; target - 1]);
        assert_eq!(ring.read(&mut output), 0);
        ring.push(&[[1, -1]]);
        assert_eq!(ring.read(&mut output), 1);
        ring.push(&vec![[2, -2]; threshold - ring.len]);
        assert_eq!(ring.len, threshold);
        ring.push(&[[3, -3]]);
        assert_eq!(ring.len, target);
        let mut drain = vec![[0; 2]; target + 1];
        assert_eq!(ring.read(&mut drain), target);
        ring.push(&[[4, -4]]);
        assert_eq!(ring.read(&mut output), 0);
    }
}

#[test]
fn mute_resets_converter_history_before_playback_resumes() {
    let mut host = HostAudio::new();
    host.sink.configure(44100);
    host.sink.active.store(true, Ordering::Relaxed);
    host.sink.push(&[[i16::MIN, i16::MAX]; 17]);
    host.toggle_mute();
    host.sink.push(&[[123, -123]; 9]);
    host.toggle_mute();
    host.sink.push(&[[1000, -1000]; 480]);
    let mut ring = host.sink.ring.lock().unwrap();
    assert_eq!(ring.len, 441);
    ring.primed = true;
    let mut output = vec![[0; 2]; ring.len];
    ring.read(&mut output);
    assert!(output.iter().all(|frame| *frame == [1000, -1000]));
}

#[test]
fn speed_mute_discards_samples_and_converter_history_before_fresh_prefill() {
    let mut host = HostAudio::new();
    host.sink.active.store(true, Ordering::Relaxed);
    host.sink.configure(44_100);
    host.sink.push(&vec![[1234, -1234]; TARGET]);
    assert!(host.sink.ring.lock().unwrap().len > 0);
    host.set_speed_muted(true);
    assert!(host.muted());
    assert_eq!(host.sink.ring.lock().unwrap().len, 0);
    host.sink.push(&vec![[1234, -1234]; TARGET]);
    host.set_speed_muted(false);
    assert!(!host.muted());
    assert!(host.sink.converter.lock().unwrap().previous.is_none());
    let mut output = [99i16; 2];
    host.sink.read(&mut output, 2);
    assert_eq!(output, [0, 0]);
    host.sink.push(&[[42, -42]]);
    host.sink.read(&mut output, 2);
    assert_eq!(output, [0, 0]);
    host.sink.push(&vec![[42, -42]; TARGET]);
    host.sink.read(&mut output, 2);
    assert_eq!(output, [42, -42]);
}

#[test]
fn speed_return_to_normal_preserves_the_users_mute_choice() {
    let mut host = HostAudio::new();
    host.sink.active.store(true, Ordering::Relaxed);
    host.toggle_mute();
    host.set_speed_muted(true);
    host.set_speed_muted(false);
    assert!(host.muted());
    host.toggle_mute();
    assert!(!host.muted());
    host.set_speed_muted(true);
    host.toggle_mute();
    host.toggle_mute();
    assert!(host.muted());
    host.set_speed_muted(false);
    assert!(!host.muted());
}

/// Signed noise that swings between the extremes, from a fixed seed.
fn noise(frames: usize) -> Vec<StereoFrame> {
    let mut seed = 0x1234_5678u32;
    (0..frames)
        .map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            [(seed >> 16) as i16, seed as i16]
        })
        .collect()
}

/// FNV-1a over the little-endian samples, left then right.
fn digest(frames: &[StereoFrame]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in frames
        .iter()
        .flatten()
        .flat_map(|sample| sample.to_le_bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// What the host callback would play for `input` drained at 48 kHz and a
/// device at `rate`, taken out of the ring after every block so that no
/// trim drops a frame.
fn played(host: &HostAudio, rate: u32, input: &[StereoFrame]) -> Vec<StereoFrame> {
    host.configure_for_test(rate);
    host.activate_for_test();
    let mut output = Vec::new();
    irregular_blocks(input, |block| {
        host.push(block);
        output.extend(host.drain_for_test());
    });
    output
}

/// The ring's frames for a 48 kHz source, pinned from a recording made with
/// the converter that knew only 48 kHz: the PC's sound reaches the speakers
/// bit for bit as it did.
#[test]
fn a_48_khz_source_plays_the_frames_it_played_before_any_rate_was_accepted() {
    let recorded: [(u32, usize, u64, usize, u64); 6] = [
        (
            16_000,
            32_000,
            18287087826510462949,
            32_000,
            8693382142447324917,
        ),
        (
            22_050,
            44_100,
            14373503324973500901,
            44_100,
            1365147526056516051,
        ),
        (
            44_100,
            88_200,
            15460946193205101829,
            88_200,
            13010199552470723856,
        ),
        (
            48_000,
            96_000,
            2053814039326507813,
            96_000,
            16695900817931928172,
        ),
        (
            96_000,
            191_999,
            12277155712566072640,
            191_999,
            18038630400980535594,
        ),
        (
            192_000,
            383_997,
            349163439852619507,
            383_997,
            9769403852891814059,
        ),
    ];
    let tone = tone(96_000);
    let noise = noise(96_000);
    let mut actual = Vec::new();
    for (rate, ..) in recorded {
        let host = HostAudio::new();
        let tone = played(&host, rate, &tone);
        let noise = played(&host, rate, &noise);
        actual.push((rate, tone.len(), digest(&tone), noise.len(), digest(&noise)));
    }
    assert_eq!(actual, recorded);
}

/// Ten seconds of a 1 kHz stereo tone drained at `source` and converted for a
/// device at `rate`: after every block exactly the frames whose place in the
/// input has been reached, and each one on the ideal tone at the device's rate.
fn check_source(source: Rate, rate: u32) {
    let (n, d) = (u128::from(source.numerator), u128::from(source.denominator));
    let r = u128::from(rate);
    let frames = (10 * n / d) as usize;
    let seconds_per_frame = d as f64 / n as f64;
    let input: Vec<StereoFrame> = (0..frames)
        .map(|index| {
            let phase = std::f64::consts::TAU * 1000.0 * index as f64 * seconds_per_frame;
            [
                (12000.0 * phase.sin()).round() as i16,
                (9000.0 * phase.cos()).round() as i16,
            ]
        })
        .collect();
    let mut converter = RateConverter::new(source, rate);
    let mut output = Vec::new();
    let mut consumed = 0u128;
    irregular_blocks(&input, |block| {
        converter.process(block, |frame| output.push(frame));
        consumed += block.len() as u128;
        // Output k lies k·n/(r·d) input frames in; the last one reached is
        // the newest input frame's.
        let expected = if consumed == 0 {
            0
        } else {
            (consumed - 1) * r * d / n + 1
        };
        assert_eq!(output.len() as u128, expected, "{source:?} to {rate} Hz");
    });
    // Linear interpolation's worst error on a sine is its amplitude times
    // (ωT)²/8, T being the input period; two more for rounding.
    let omega_t = std::f64::consts::TAU * 1000.0 * seconds_per_frame;
    let tolerance = 12000.0 * omega_t * omega_t / 8.0 + 2.0;
    for (index, frame) in output.iter().enumerate() {
        let phase = std::f64::consts::TAU * 1000.0 * index as f64 / f64::from(rate);
        for (sample, expected) in frame
            .iter()
            .zip([12000.0 * phase.sin(), 9000.0 * phase.cos()])
        {
            assert!(
                (f64::from(*sample) - expected).abs() <= tolerance,
                "{source:?} to {rate} Hz: frame {index} is {sample}, the tone {expected}"
            );
        }
    }
    // The tone's frequency, counted: one rising zero crossing of the left
    // channel per period over the output's duration.
    let rising = output
        .windows(2)
        .filter(|pair| pair[0][0] < 0 && pair[1][0] >= 0)
        .count();
    let periods = 1000.0 * output.len() as f64 / f64::from(rate);
    assert!(
        (rising as f64 - periods).abs() <= 1.0,
        "{source:?} to {rate} Hz: {rising} periods in {periods}"
    );
}

#[test]
fn the_nes_rate_reaches_the_device_with_every_frame_and_the_tone_kept() {
    check_source(Rate::new(984_375, 22), 48_000);
    check_source(Rate::new(984_375, 22), 44_100);
}

#[test]
fn whole_odd_rates_and_the_bounds_reach_the_device_with_the_tone_kept() {
    check_source(Rate::new(44_100, 1), 48_000);
    check_source(Rate::new(192_000, 1), 44_100);
    check_source(Rate::new(32_000, 1), 48_000);
}

#[test]
fn the_window_plays_exactly_the_rates_the_contract_allows() {
    for (rate, played) in [
        (Rate::new(8_000, 1), true),
        (Rate::new(7_999, 1), false),
        (Rate::new(192_000, 1), true),
        (Rate::new(192_001, 1), false),
        (Rate::new(984_375, 22), true),
        (Rate::new(15_999, 2), false),
        (Rate::new(384_000, 2), true),
        (
            Rate {
                numerator: 48_000,
                denominator: 0,
            },
            false,
        ),
        (Rate::new(0, 1), false),
        (Rate::new(u64::MAX, u64::MAX - 1), false),
    ] {
        assert_eq!(plays(rate), played, "{rate:?}");
    }
    let mut host = HostAudio::for_source(Rate::new(4_000, 1));
    let mut warnings = Vec::new();
    host.open(&mut warnings);
    assert_eq!(
        String::from_utf8(warnings).unwrap(),
        "audio disabled: the machine's sound runs at 4000 Hz; the window plays 8000 to 192000 Hz\n"
    );
    assert_eq!(host.source(), Rate::new(4_000, 1), "a refused rate is kept");
    assert_eq!(
        HostAudio::for_source(Rate::new(984_375, 22)).source(),
        Rate::new(984_375, 22)
    );
    assert_eq!(HostAudio::new().source(), PC_RATE);
}

/// A machine's 48 kHz, stated in its description, plays as the default did.
#[test]
fn a_described_48_khz_source_plays_what_the_default_plays() {
    let input = noise(9_600);
    for rate in [44_100, 48_000] {
        let default = played(&HostAudio::new(), rate, &input);
        let described = played(&HostAudio::for_source(PC_RATE), rate, &input);
        assert_eq!(default, described);
    }
}

#[test]
fn a_device_at_the_sources_own_rate_is_preferred_and_plays_it_unconverted() {
    let ranges = [
        range(2, 48000, SampleFormat::I16),
        range(2, 44100, SampleFormat::I16),
    ];
    assert_eq!(
        select_config(&ranges, None, 44_100).unwrap().sample_rate(),
        44_100
    );
    let source = Rate::new(44_100, 1);
    let host = HostAudio::for_source(source);
    host.configure_for_test(44_100);
    host.activate_for_test();
    let input = noise(441);
    host.push(&input);
    assert_eq!(host.drain_for_test(), input);
}
