// SPDX-License-Identifier: AGPL-3.0-or-later
//! The screen's geometry and its inverse, without a window.

use super::*;
use unemu_core_api::pointer_pixel;

/// A picture of `width` × `height` with `aspect` and `visible`; the
/// geometry never reads the pixels.
fn picture(width: u32, height: u32, aspect: (u32, u32), visible: FrameArea) -> Picture {
    Picture {
        width,
        height,
        pixels: Vec::new(),
        aspect,
        serial: 0,
        visible,
    }
}

fn area(x: u32, y: u32, width: u32, height: u32) -> FrameArea {
    FrameArea {
        x,
        y,
        width,
        height,
    }
}

fn panel(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// A NES-like picture: 256 × 240 of 8:7 pixels, the top and bottom eight
/// rows overscan.
fn nes() -> Picture {
    picture(256, 240, (128, 105), area(0, 8, 256, 224))
}

fn close(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-3
}

#[test]
fn hidden_overscan_shows_the_visible_part_with_the_pixels_kept_in_shape() {
    let layout = layout(&nes(), panel(0.0, 0.0, 1000.0, 448.0), false, false).unwrap();
    assert_eq!(layout.source, area(0, 8, 256, 224));
    // 128:105 × (256/256) ÷ (224/240) is 64:49, as tall as the panel.
    assert!(close(layout.dest.height, 448.0), "{layout:?}");
    assert!(close(layout.dest.width, 448.0 * 64.0 / 49.0), "{layout:?}");
    assert!(close(layout.dest.x, (1000.0 - 448.0 * 64.0 / 49.0) / 2.0));
    assert!(close(layout.dest.y, 0.0));
}

#[test]
fn shown_overscan_shows_the_whole_picture_at_its_aspect() {
    let layout = layout(&nes(), panel(10.0, 20.0, 1280.0, 2000.0), true, false).unwrap();
    assert_eq!(layout.source, area(0, 0, 256, 240));
    // As wide as the panel, 128:105, centred down it.
    assert!(close(layout.dest.width, 1280.0), "{layout:?}");
    assert!(close(layout.dest.height, 1050.0), "{layout:?}");
    assert!(close(layout.dest.x, 10.0));
    assert!(close(layout.dest.y, 20.0 + (2000.0 - 1050.0) / 2.0));
}

#[test]
fn integer_scaling_makes_the_height_a_whole_multiple_of_the_shown_rows() {
    let hidden = layout(&nes(), panel(0.0, 0.0, 2000.0, 700.0), false, true).unwrap();
    assert!(close(hidden.dest.height, 672.0), "{hidden:?}");
    assert!(close(hidden.dest.width, 672.0 * 64.0 / 49.0));
    let shown = layout(&nes(), panel(0.0, 0.0, 2000.0, 700.0), true, true).unwrap();
    assert!(close(shown.dest.height, 480.0), "{shown:?}");
    assert!(close(shown.dest.width, 480.0 * 128.0 / 105.0));
    // Narrow panels limit the width first; the height still steps.
    let narrow = layout(&nes(), panel(0.0, 0.0, 400.0, 2000.0), true, true).unwrap();
    assert!(close(narrow.dest.height, 240.0), "{narrow:?}");
    // A panel lower than the rows shows them smaller.
    let small = layout(&nes(), panel(0.0, 0.0, 2000.0, 100.0), false, true).unwrap();
    assert!(close(small.dest.height, 100.0), "{small:?}");
}

#[test]
fn a_four_by_three_picture_fills_the_panel_as_the_pcs_screen_does() {
    let pc = picture(640, 400, (4, 3), area(0, 0, 640, 400));
    for (width, height) in [(1000.0, 600.0), (600.0, 1000.0), (800.0, 600.0)] {
        let layout = layout(&pc, panel(0.0, 0.0, width, height), false, false).unwrap();
        let expected = (
            f32::min(width, height * 4.0 / 3.0),
            f32::min(height, width * 3.0 / 4.0),
        );
        assert!(close(layout.dest.width, expected.0), "{layout:?}");
        assert!(close(layout.dest.height, expected.1), "{layout:?}");
    }
}

#[test]
fn nothing_is_shown_for_an_empty_picture_panel_or_aspect() {
    let empty = picture(0, 240, (4, 3), area(0, 0, 0, 240));
    assert_eq!(
        layout(&empty, panel(0.0, 0.0, 10.0, 10.0), false, false),
        None
    );
    assert_eq!(
        layout(&nes(), panel(0.0, 0.0, 0.0, 10.0), false, false),
        None
    );
    assert_eq!(
        layout(&nes(), panel(0.0, 0.0, f32::NAN, 10.0), false, false),
        None
    );
    let flat = picture(256, 240, (0, 1), area(0, 0, 256, 240));
    assert_eq!(
        layout(&flat, panel(0.0, 0.0, 10.0, 10.0), false, false),
        None
    );
    // A visible part outside the picture is no statement: all of it shows.
    let wild = picture(256, 240, (4, 3), area(200, 0, 100, 240));
    let layout = layout(&wild, panel(0.0, 0.0, 10.0, 10.0), false, false).unwrap();
    assert_eq!(layout.source, area(0, 0, 256, 240));
}

/// The pixel of the whole picture a point of the panel means.
fn pixel_under(layout: &ScreenLayout, x: f32, y: f32) -> Option<(u32, u32)> {
    let (width, height) = layout.picture;
    layout
        .pointer_position(x, y)
        .map(|place| pointer_pixel(place, width, height))
}

#[test]
fn the_pointer_maps_to_the_whole_picture_with_overscan_hidden_and_shown() {
    // Each shown pixel covers about 2.3 × 2 points of a panel away from the
    // window's corner.
    let whole = nes();
    let hidden = layout(&whole, panel(50.0, 30.0, 2000.0, 448.0), false, false).unwrap();
    let (left, top) = (hidden.dest.x, hidden.dest.y);
    let column = hidden.dest.width / 256.0;
    assert_eq!(
        pixel_under(&hidden, left, top),
        Some((0, 8)),
        "first visible"
    );
    assert_eq!(
        pixel_under(&hidden, left + hidden.dest.width - 0.01, top + 447.99),
        Some((255, 231)),
        "last visible"
    );
    assert_eq!(
        pixel_under(&hidden, left + 100.5 * column, top + 2.0 * 50.5),
        Some((100, 58))
    );
    let shown = layout(&whole, panel(50.0, 30.0, 2000.0, 480.0), true, false).unwrap();
    let (left, top) = (shown.dest.x, shown.dest.y);
    let column = shown.dest.width / 256.0;
    assert_eq!(pixel_under(&shown, left, top), Some((0, 0)));
    assert_eq!(
        pixel_under(&shown, left + 100.5 * column, top + 2.0 * 50.5),
        Some((100, 50))
    );
    assert_eq!(
        pixel_under(&shown, left + shown.dest.width - 0.01, top + 479.99),
        Some((255, 239))
    );
    // Every pixel's centre maps back to it, also for odd sizes.
    for shape in [
        nes(),
        picture(7, 5, (3, 2), area(1, 1, 5, 3)),
        picture(4096, 3, (1, 1), area(0, 0, 4096, 3)),
    ] {
        for show in [false, true] {
            let layout = layout(&shape, panel(3.0, 4.0, 900.0, 700.0), show, false).unwrap();
            let source = layout.source;
            let (step_x, step_y) = (
                layout.dest.width / source.width as f32,
                layout.dest.height / source.height as f32,
            );
            for row in 0..source.height {
                for column in (0..source.width).step_by(97) {
                    let x = layout.dest.x + (column as f32 + 0.5) * step_x;
                    let y = layout.dest.y + (row as f32 + 0.5) * step_y;
                    assert_eq!(
                        pixel_under(&layout, x, y),
                        Some((source.x + column, source.y + row)),
                        "{shape:?} {show}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_pointer_off_the_shown_rectangle_is_nowhere() {
    let layout = layout(&nes(), panel(0.0, 0.0, 2000.0, 448.0), false, false).unwrap();
    let dest = layout.dest;
    let right = dest.x + dest.width;
    let bottom = dest.y + dest.height;
    for (x, y) in [
        (dest.x - 0.01, dest.y + 10.0),
        (right, dest.y + 10.0),
        (dest.x + 10.0, dest.y - 0.01),
        (dest.x + 10.0, bottom),
        (0.0, 0.0),
        (f32::NAN, dest.y + 10.0),
    ] {
        assert_eq!(layout.pointer_position(x, y), None, "({x}, {y})");
    }
    assert!(layout.pointer_position(dest.x, dest.y).is_some());
}
