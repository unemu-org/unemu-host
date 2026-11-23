// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a machine's picture goes in a panel of the window, and back from a
//! point of the panel to a place on the picture (core-contract.md §13.3,
//! "The picture" and "The absolute pointer").
//!
//! A view asks [`layout`] for the part of the picture to draw and the
//! rectangle to draw it in, and asks the result for the place under the host
//! pointer ([`ScreenLayout::pointer_position`]). Both are pure: no toolkit
//! type and no host state, so every view of a running machine shows it the
//! same way and a test checks the arithmetic without a window.
//!
//! - Overscan hidden, the default, shows only the picture's visible part
//!   ([`Picture::visible`]); shown, the whole picture.
//! - The shown rectangle has the shape `aspect × (shown width / width) ÷
//!   (shown height / height)`, so the pixels keep the shape the machine
//!   states for the whole picture when the view crops, and it is as large
//!   as the panel allows, centred in it.
//! - With integer scaling on, its height is a whole multiple of the shown
//!   rows (a panel lower than the rows shows them smaller instead).
//!
//! The PC's screen keeps its own 4:3 path in the shell; this is the
//! geometry of every other machine.

use crate::session::Picture;
use unemu_core_api::FrameArea;

/// A rectangle of the window, in the toolkit's logical points: `x` from the
/// left edge and `y` from the top edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// How one picture is shown in a panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenLayout {
    /// The part of the picture shown, in its pixels.
    pub source: FrameArea,
    /// Where that part is drawn, in the panel's coordinates.
    pub dest: Rect,
    /// The whole picture's width and height, which pointer places are
    /// fractions of.
    picture: (u32, u32),
}

/// Where `picture` goes in `panel`. `None` when there is nothing to show: an
/// empty picture or panel, or an aspect with a zero side.
pub fn layout(
    picture: &Picture,
    panel: Rect,
    show_overscan: bool,
    integer_scaling: bool,
) -> Option<ScreenLayout> {
    let (width, height) = (picture.width, picture.height);
    let (aspect_x, aspect_y) = picture.aspect;
    let usable = |side: f32| side.is_finite() && side > 0.0;
    if width == 0
        || height == 0
        || aspect_x == 0
        || aspect_y == 0
        || !usable(panel.width)
        || !usable(panel.height)
    {
        return None;
    }
    let whole = FrameArea {
        x: 0,
        y: 0,
        width,
        height,
    };
    // The field is public, so a visible part outside the picture is taken
    // as no statement at all.
    let visible = picture.visible;
    let source = if show_overscan
        || visible.width == 0
        || visible.height == 0
        || !visible.is_within(width, height)
    {
        whole
    } else {
        visible
    };
    // The shown shape, width to height, as an exact fraction.
    let shape_x = f64::from(aspect_x) * f64::from(source.width) * f64::from(height);
    let shape_y = f64::from(aspect_y) * f64::from(source.height) * f64::from(width);
    let (panel_width, panel_height) = (f64::from(panel.width), f64::from(panel.height));
    let (mut dest_width, mut dest_height) = if panel_width * shape_y <= panel_height * shape_x {
        (panel_width, panel_width * shape_y / shape_x)
    } else {
        (panel_height * shape_x / shape_y, panel_height)
    };
    if integer_scaling {
        let rows = f64::from(source.height);
        let times = (dest_height / rows).floor();
        if times >= 1.0 {
            dest_height = times * rows;
            dest_width = dest_height * shape_x / shape_y;
        }
    }
    Some(ScreenLayout {
        source,
        dest: Rect {
            x: (f64::from(panel.x) + (panel_width - dest_width) / 2.0) as f32,
            y: (f64::from(panel.y) + (panel_height - dest_height) / 2.0) as f32,
            width: dest_width as f32,
            height: dest_height as f32,
        },
        picture: (width, height),
    })
}

impl ScreenLayout {
    /// The place on the whole picture under the point `(x, y)` of the panel,
    /// as [`unemu_core_api::InputEvent::PointerPosition`] carries it; `None`
    /// off the shown rectangle.
    ///
    /// The place is the centre of the pixel under the point, so
    /// [`unemu_core_api::pointer_pixel`] gives that pixel back for any
    /// picture up to 32 768 pixels each way. With overscan hidden the pixel
    /// lies in the visible part, which is all the rectangle shows.
    pub fn pointer_position(&self, x: f32, y: f32) -> Option<(u16, u16)> {
        let along = |point: f32, start: f32, length: f32, first: u32, count: u32, whole: u32| {
            let offset = f64::from(point) - f64::from(start);
            let length = f64::from(length);
            // Written so that a NaN point falls outside too.
            if !(offset >= 0.0 && offset < length) {
                return None;
            }
            let shown = ((offset / length * f64::from(count)).floor() as u32).min(count - 1);
            let pixel = u64::from(first + shown);
            Some(((2 * pixel + 1) << 16) / (2 * u64::from(whole)))
        };
        let column = along(
            x,
            self.dest.x,
            self.dest.width,
            self.source.x,
            self.source.width,
            self.picture.0,
        )?;
        let row = along(
            y,
            self.dest.y,
            self.dest.height,
            self.source.y,
            self.source.height,
            self.picture.1,
        )?;
        Some((column as u16, row as u16))
    }
}

#[cfg(test)]
mod tests;
