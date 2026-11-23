// SPDX-License-Identifier: AGPL-3.0-or-later
//! The exact frame count of the sound: at every return of `run`, once
//! `drain_audio` was called, the next frame has index
//! `floor(now × rate / clock)`, also across sound off and on and across
//! `load_state`; and frame `i` is the sound at `i × clock / rate` ticks.

use crate::support::{create, drain, run_to};
use unemu_core_api::{Machine, Outputs, Rate, Ticks};
use unemu_test_core::sound::{frame_index, sample, AMPLITUDE};

/// 21 477 272/4 ticks a second against 984 375/22 frames a second: neither
/// divides the other, so frame edges fall between ticks.
const ODD: &str = "unemu-test-core 1
clock 21477272/4
sound 984375/22 440
saved-state
";

fn expected(now: u64) -> u64 {
    // floor(now × (984375/22) / (21477272/4)) = floor(now × 984375 × 4 / (22 × 21477272)).
    (u128::from(now) * 984_375 * 4 / (22 * 21_477_272)) as u64
}

/// The tick at which frame `index` of the odd machine starts: the first
/// tick at or after `index × clock / rate`.
fn edge(index: u64) -> u64 {
    let numerator = u128::from(index) * 21_477_272 * 22;
    let denominator = 4 * 984_375u128;
    numerator.div_ceil(denominator) as u64
}

fn count(machine: &mut dyn Machine, total: &mut u64) {
    *total += drain(machine).len() as u64;
}

#[test]
fn the_next_frame_is_the_floor_of_now_times_rate_over_clock() {
    let mut machine = create(ODD);
    let mut total = 0;
    for index in [
        1u64, 2, 3, 7, 22, 100, 1_000, 9_600, 44_744, 100_001, 447_443,
    ] {
        let at = edge(index);
        for deadline in [at - 1, at, at + 1] {
            run_to(machine.as_mut(), deadline);
            assert_eq!(machine.now(), Ticks(deadline));
            count(machine.as_mut(), &mut total);
            assert_eq!(total, expected(deadline), "deadline {deadline}");
            assert_eq!(
                total,
                frame_index(
                    machine.now(),
                    Rate::new(21_477_272, 4),
                    Rate::new(984_375, 22)
                )
            );
        }
        assert_eq!(
            expected(at - 1),
            index - 1,
            "frame {index} starts at tick {at}"
        );
        assert_eq!(expected(at), index);
    }
}

#[test]
fn sound_switched_off_drops_its_frames_and_comes_back_at_now() {
    let mut machine = create(ODD);
    run_to(machine.as_mut(), 1_000);
    machine.set_outputs(Outputs::NONE);
    run_to(machine.as_mut(), 50_000);
    assert!(
        drain(machine.as_mut()).is_empty(),
        "nothing while sound is off"
    );
    machine.set_outputs(Outputs::NONE.with_audio(true));
    run_to(machine.as_mut(), 90_001);
    let frames = drain(machine.as_mut());
    assert_eq!(frames.len() as u64, expected(90_001) - expected(50_000));
    // The frames keep their place in time: they are the frames from the one
    // at tick 50 000 on.
    let first = expected(50_000);
    let rate = Rate::new(984_375, 22);
    let tone = Rate::new(440, 1);
    for (offset, frame) in frames.iter().enumerate() {
        assert_eq!(*frame, sample(first + offset as u64, rate, tone));
    }
    // Undrained frames are dropped when sound goes off, also when it was
    // not drained in between.
    run_to(machine.as_mut(), 120_000);
    machine.set_outputs(Outputs::NONE);
    machine.set_outputs(Outputs::NONE.with_audio(true));
    run_to(machine.as_mut(), 130_000);
    assert_eq!(
        drain(machine.as_mut()).len() as u64,
        expected(130_000) - expected(120_000)
    );
}

#[test]
fn after_load_state_the_index_follows_the_loaded_time() {
    let mut machine = create(ODD);
    run_to(machine.as_mut(), 200_000);
    drain(machine.as_mut());
    let mut state = Vec::new();
    machine.save_state(&mut state).expect("save");
    run_to(machine.as_mut(), 700_000);
    // Frames up to 700 000 were never drained; loading drops them.
    machine.load_state(&mut state.as_slice()).expect("load");
    assert_eq!(machine.now(), Ticks(200_000));
    assert!(drain(machine.as_mut()).is_empty());
    run_to(machine.as_mut(), 300_007);
    assert_eq!(
        drain(machine.as_mut()).len() as u64,
        expected(300_007) - expected(200_000)
    );
    // Loading a later state jumps forward: the index follows again.
    run_to(machine.as_mut(), 900_000);
    let mut later = Vec::new();
    machine.save_state(&mut later).expect("save");
    machine
        .load_state(&mut state.as_slice())
        .expect("back to 200 000");
    drain(machine.as_mut());
    machine
        .load_state(&mut later.as_slice())
        .expect("forward to 900 000");
    run_to(machine.as_mut(), 900_500);
    assert_eq!(
        drain(machine.as_mut()).len() as u64,
        expected(900_500) - expected(900_000)
    );
}

#[test]
fn the_wave_is_a_square_of_the_tone() {
    // 48 000 frames a second at 1 000 Hz: 24 frames up, 24 down.
    let mut machine = create("unemu-test-core 1\nclock 48000\nsound 48000 1000\n");
    run_to(machine.as_mut(), 96);
    let frames = drain(machine.as_mut());
    assert_eq!(frames.len(), 96);
    for (index, frame) in frames.iter().enumerate() {
        let value = if index % 48 < 24 {
            AMPLITUDE
        } else {
            -AMPLITUDE
        };
        assert_eq!(*frame, [value, value], "frame {index}");
    }
}
