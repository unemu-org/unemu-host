// SPDX-License-Identifier: AGPL-3.0-or-later
//! The test machine's sound: a square wave that keeps its place in virtual
//! time, so a test can count frames and measure the tone exactly.

use unemu_core_api::{Rate, Ticks};

/// The square wave's height: every frame is `+AMPLITUDE` or `-AMPLITUDE`
/// in both samples.
pub const AMPLITUDE: i16 = 8192;

/// The index of the next frame a machine hands over at `now`: the floor of
/// `now × rate / clock`.
pub fn frame_index(now: Ticks, clock: Rate, rate: Rate) -> u64 {
    let numerator = u128::from(now.0) * u128::from(rate.numerator) * u128::from(clock.denominator);
    let denominator = u128::from(rate.denominator) * u128::from(clock.numerator);
    u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
}

/// Frame `index` of a square wave of frequency `tone` at `rate` frames per
/// second: `+AMPLITUDE` in the first half of each period, which starts at
/// frame 0, and `-AMPLITUDE` in the second.
///
/// The half period a frame falls in is the floor of
/// `index × 2 × tone / rate`; it repeats with the frame index modulo twice
/// the denominator of that fraction, which keeps the product within 128 bits
/// for the rates a script may give.
pub fn sample(index: u64, rate: Rate, tone: Rate) -> [i16; 2] {
    let scale = 2 * u128::from(tone.numerator) * u128::from(rate.denominator);
    let per = u128::from(tone.denominator) * u128::from(rate.numerator);
    let half = (u128::from(index) % (2 * per)) * scale / per;
    let value = if half % 2 == 0 { AMPLITUDE } else { -AMPLITUDE };
    [value, value]
}
