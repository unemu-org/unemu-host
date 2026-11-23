// SPDX-License-Identifier: AGPL-3.0-or-later
//! The test machine's picture, as a test reads it back.
//!
//! The picture is drawn when a frame finishes, from what the machine held at
//! that moment, so a test sees an input only after the next frame edge:
//!
//! - Outside the visible part every pixel is [`OVERSCAN`].
//! - Inside it every pixel is [`background`] of the frames finished since
//!   the machine last started (creation, a reset or a reset by the guest).
//! - The field, [`field_area`] of the visible part, is [`field_colour`] of
//!   the pad controls held then.
//! - Each pointer that points at the picture draws a cross of five pixels
//!   across and five down, centred on its pixel, over the whole picture
//!   (overscan included), in [`cross_colour`] of its held buttons.

use crate::script::CONTROLS;
use unemu_core_api::{FrameArea, PadControl};

/// The colour of every pixel outside the visible part.
pub const OVERSCAN: u32 = 0x00FF_00FF;

/// How far a cross reaches from its centre, in pixels.
pub const CROSS_REACH: u32 = 2;

/// The colour of the visible part's background after `frames` finished
/// frames: their count, in the low 24 bits.
pub const fn background(frames: u64) -> u32 {
    (frames & 0x00FF_FFFF) as u32
}

/// The field inside a visible part: the middle half of it each way, at
/// least one pixel.
pub const fn field_area(visible: FrameArea) -> FrameArea {
    let width = if visible.width / 2 == 0 {
        1
    } else {
        visible.width / 2
    };
    let height = if visible.height / 2 == 0 {
        1
    } else {
        visible.height / 2
    };
    FrameArea {
        x: visible.x + visible.width / 4,
        y: visible.y + visible.height / 4,
        width,
        height,
    }
}

/// The bit of a control within one player's twelve bits: the order of
/// [`PadControl`]'s variants, `Up` in bit 0 to `Select` in bit 11.
pub fn control_bit(control: PadControl) -> u16 {
    CONTROLS
        .iter()
        .position(|(known, _)| *known == control)
        .map_or(0, |index| 1 << index)
}

/// The field's colour for the controls each player holds: the first pad
/// port's twelve bits in bits 0 to 11, the second's in bits 12 to 23. A
/// third pad port and later ones do not show.
pub fn field_colour(held: &[&[PadControl]]) -> u32 {
    let masks: Vec<u16> = held
        .iter()
        .map(|controls| controls.iter().fold(0, |mask, &c| mask | control_bit(c)))
        .collect();
    field_from_masks(&masks)
}

pub(crate) fn field_from_masks(masks: &[u16]) -> u32 {
    let first = u32::from(masks.first().copied().unwrap_or(0));
    let second = u32::from(masks.get(1).copied().unwrap_or(0));
    first | second << 12
}

/// The colour of a pointer's cross: white while no button is held,
/// otherwise red with the held buttons' mask (button 0 in bit 0) in the
/// blue byte.
pub const fn cross_colour(buttons: u8) -> u32 {
    if buttons == 0 {
        0x00FF_FFFF
    } else {
        0x00FF_0000 | buttons as u32
    }
}

/// What a finished frame shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub frames: u64,
    /// The first two pad ports' held controls: the ones the field shows.
    pub pads: Vec<u16>,
    /// Each pointer's pixel, if it points at the picture, and its buttons.
    pub pointers: Vec<crate::state::Pointer>,
}

/// Draw `shown` into `pixels`, a picture of `width` × `height`.
pub(crate) fn draw(
    pixels: &mut Vec<u32>,
    width: u32,
    height: u32,
    visible: FrameArea,
    shown: &Shown,
) {
    let (w, h) = (width as usize, height as usize);
    pixels.clear();
    pixels.resize(w * h, OVERSCAN);
    let colour = background(shown.frames);
    fill(pixels, w, visible, colour);
    fill(
        pixels,
        w,
        field_area(visible),
        field_from_masks(&shown.pads),
    );
    for &(place, buttons) in &shown.pointers {
        let Some((x, y)) = place else { continue };
        let colour = cross_colour(buttons);
        let reach = CROSS_REACH as i64;
        for d in -reach..=reach {
            let column = i64::from(x) + d;
            if (0..i64::from(width)).contains(&column) {
                pixels[y as usize * w + column as usize] = colour;
            }
            let row = i64::from(y) + d;
            if (0..i64::from(height)).contains(&row) {
                pixels[row as usize * w + x as usize] = colour;
            }
        }
    }
}

fn fill(pixels: &mut [u32], width: usize, area: FrameArea, colour: u32) {
    for row in area.y..area.y + area.height {
        let start = row as usize * width + area.x as usize;
        pixels[start..start + area.width as usize].fill(colour);
    }
}
