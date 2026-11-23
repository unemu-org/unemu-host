// SPDX-License-Identifier: AGPL-3.0-or-later
//! The session over a whole core: the scripted test core, started as the
//! generic launch path starts one and driven through the contract alone
//! (core-contract.md §13.3, the tests of `machine-view`).

use super::*;
use crate::screen::{self, Rect};
use std::rc::Rc;
use unemu_core_api::test_support::{MemoryContent, MemoryStorage};
use unemu_core_api::{
    Content, ContentSource, Core, CreateRequest, Error, OfferedContent, Rate, Status,
};
use unemu_test_core::picture::{background, cross_colour, field_area, field_colour, OVERSCAN};
use unemu_test_core::sound::{frame_index, sample};
use unemu_test_core::{TestCore, BATTERY_RECORD};
use winit::event::ElementState::{Pressed, Released};

/// A digest standing for a script's bytes: the core trusts the one the
/// front end gives, and two scripts get two.
fn digest_of(script: &str) -> [u8; 32] {
    let mut digest = [0u8; 32];
    for (index, byte) in script.bytes().enumerate() {
        digest[index % 32] = digest[index % 32].wrapping_mul(31).wrapping_add(byte);
    }
    digest
}

struct Source(Vec<u8>);

impl ContentSource for Source {
    fn open(&self, sha256: &[u8; 32], size: u64) -> Result<Content, Error> {
        let script = std::str::from_utf8(&self.0).unwrap();
        assert_eq!((*sha256, size), (digest_of(script), self.0.len() as u64));
        Ok(Content::Bytes(self.0.clone()))
    }
}

/// A machine for `script` over `storage`: identify, validate, create.
fn machine_on(script: &str, storage: &MemoryStorage) -> Box<dyn Machine> {
    let core: &dyn Core = &TestCore;
    let content = MemoryContent::new(script.as_bytes().to_vec());
    let identified = core
        .identify(&OfferedContent::new(
            "test.utc",
            digest_of(script),
            script.len() as u64,
            &content,
        ))
        .unwrap()
        .expect("the test core runs the script");
    let config = identified.config.as_document();
    core.validate(identified.platform, config).unwrap();
    core.create(&CreateRequest::new(
        identified.platform,
        config,
        &Source(script.as_bytes().to_vec()),
        Rc::new(storage.clone()),
    ))
    .unwrap()
}

fn session_on(script: &str, storage: &MemoryStorage) -> Session {
    Session::new(machine_on(script, storage), DisplayId(0), &mut io::sink()).unwrap()
}

fn session(script: &str) -> Session {
    session_on(script, &MemoryStorage::new())
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// The events that reached a machine with `echo-input`, from the messages a
/// session wrote, in `Debug` form.
fn echoed(messages: &[u8]) -> Vec<String> {
    String::from_utf8(messages.to_vec())
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("input at "))
        .map(|line| line.split_once(": ").unwrap().1.to_string())
        .collect()
}

fn pixel(session: &Session, x: u32, y: u32) -> u32 {
    let picture = session.picture().unwrap();
    picture.pixels[(y * picture.width + x) as usize]
}

/// Ten virtual seconds of the test core's square wave at `rate`, through
/// the session to a device at 48 kHz, taken out of the ring after every
/// advance; and the same tone made here and converted in one go.
fn ten_seconds_of_sound(rate: &str, tone: Rate) -> (Vec<[i16; 2]>, Vec<[i16; 2]>, u64) {
    let script = format!("unemu-test-core 1\nclock 1000000\nsound {rate} 1000\n");
    let mut session = session(&script);
    let source = session.audio.source();
    session.audio.configure_for_test(48_000);
    session.audio.activate_for_test();
    let mut played = Vec::new();
    for millis in (10..=10_000).step_by(10) {
        session.advance_fully(ms(millis)).unwrap();
        played.extend(session.audio.drain_for_test());
    }
    assert_eq!(session.machine().now(), Ticks(10_000_000));
    let handed = frame_index(Ticks(10_000_000), Rate::new(1_000_000, 1), source);
    let reference_audio = HostAudio::for_source(source);
    reference_audio.configure_for_test(48_000);
    reference_audio.activate_for_test();
    let ideal: Vec<_> = (0..handed)
        .map(|index| sample(index, source, tone))
        .collect();
    let mut reference = Vec::new();
    for block in ideal.chunks(441) {
        reference_audio.push(block);
        reference.extend(reference_audio.drain_for_test());
    }
    (played, reference, handed)
}

#[test]
fn sound_of_the_test_core_reaches_the_device_at_its_rate_with_no_frame_lost_or_invented() {
    let tone = Rate::new(1_000, 1);
    for (rate, exact) in [
        ("984375/22", Rate::new(984_375, 22)),
        ("44100", Rate::new(44_100, 1)),
    ] {
        let (played, reference, handed) = ten_seconds_of_sound(rate, tone);
        assert_eq!(handed, 10 * exact.numerator / exact.denominator, "{rate}");
        // Every frame the machine handed over was converted, once, in order.
        let (n, d) = (u128::from(exact.numerator), u128::from(exact.denominator));
        assert_eq!(
            played.len() as u128,
            (u128::from(handed) - 1) * 48_000 * d / n + 1,
            "{rate}"
        );
        assert_eq!(played, reference, "{rate}");
        // The tone keeps its frequency at the device's rate.
        let rising = played
            .windows(2)
            .filter(|pair| pair[0][0] < 0 && pair[1][0] >= 0)
            .count();
        let periods = 1000.0 * played.len() as f64 / 48_000.0;
        assert!((rising as f64 - periods).abs() <= 1.0, "{rate}: {rising}");
    }
}

#[test]
fn a_bound_key_works_the_test_cores_pad_and_an_unbound_one_reaches_nobody() {
    let mut session = session(
        "unemu-test-core 1
clock 1000000
pad 0 up,down,left,right,a,b,start,select Controller 1
echo-input
",
    );
    for (key, state) in [
        (KeyCode::KeyX, Pressed),
        (KeyCode::KeyX, Pressed),
        (KeyCode::KeyK, Pressed),
        (KeyCode::KeyK, Released),
    ] {
        session.key_event(key, state);
    }
    let mut messages = Vec::new();
    session
        .advance_fully_with_warnings(ms(2), &mut messages)
        .unwrap();
    assert_eq!(
        echoed(&messages),
        [format!(
            "{:?}",
            InputEvent::Pad {
                port: PortId(0),
                control: PadControl::A,
                pressed: true
            }
        )],
        "one pad event, no key event"
    );
    let field = field_area(session.picture().unwrap().visible);
    assert_eq!(
        pixel(&session, field.x, field.y),
        field_colour(&[&[PadControl::A]])
    );
}

const GUN: &str = "unemu-test-core 1
clock 1000000
picture 40 30
aspect 4 3
visible 4 2 32 26
absolute 2 1 Gun
echo-input
";

const PANEL: Rect = Rect {
    x: 20.0,
    y: 10.0,
    width: 640.0,
    height: 480.0,
};

/// The point of the panel at the centre of `pixel` of the whole picture.
fn point_at(layout: &screen::ScreenLayout, pixel: (u32, u32)) -> (f32, f32) {
    let source = layout.source;
    (
        layout.dest.x + (pixel.0 - source.x) as f32 * layout.dest.width / source.width as f32 + 0.5,
        layout.dest.y
            + (pixel.1 - source.y) as f32 * layout.dest.height / source.height as f32
            + 0.5,
    )
}

#[test]
fn the_pointer_aims_at_the_whole_picture_with_overscan_hidden_and_shown_and_none_outside() {
    let mut session = session(GUN);
    assert!(session.aims());
    session.mouse.pointer_inside(true);
    session.mouse_button_with_grab(MouseButton::Left, Pressed, || panic!("no capture"));
    session.mouse_button_with_grab(MouseButton::Left, Released, || panic!("no capture"));
    assert!(!session.mouse.captured());
    let mut messages = Vec::new();
    session
        .advance_fully_with_warnings(ms(1), &mut messages)
        .unwrap();

    // Overscan hidden: the visible part fills the screen.
    let picture = session.picture().unwrap().clone();
    let hidden = screen::layout(&picture, PANEL, false, false).unwrap();
    let (x, y) = point_at(&hidden, (10, 7));
    session.pointer_at(hidden.pointer_position(x, y));
    // A move within the same pixel sends nothing.
    session.pointer_at(hidden.pointer_position(x + 0.25, y + 0.25));
    session.mouse_button(MouseButton::Left, Pressed);
    session
        .advance_fully_with_warnings(ms(2), &mut messages)
        .unwrap();
    assert_eq!(pixel(&session, 10, 7), cross_colour(1));
    assert_eq!(pixel(&session, 12, 7), cross_colour(1));
    assert_eq!(pixel(&session, 13, 7), background(2));

    // Overscan shown: a place in the overscan.
    let shown = screen::layout(&picture, PANEL, true, false).unwrap();
    let (x, y) = point_at(&shown, (1, 1));
    session.pointer_at(shown.pointer_position(x, y));
    session.mouse_button(MouseButton::Left, Released);
    session
        .advance_fully_with_warnings(ms(3), &mut messages)
        .unwrap();
    assert_eq!(pixel(&session, 1, 1), cross_colour(0));
    assert_eq!(pixel(&session, 10, 7), background(3));

    // Off the screen's rectangle: nowhere.
    session.pointer_at(shown.pointer_position(PANEL.x - 1.0, y));
    session
        .advance_fully_with_warnings(ms(4), &mut messages)
        .unwrap();
    assert_eq!(pixel(&session, 1, 1), OVERSCAN);

    // Focus loss: everything released, then nowhere again.
    session.pointer_at(shown.pointer_position(x, y));
    session.mouse_button(MouseButton::Left, Pressed);
    session.focus_changed(false);
    session
        .advance_fully_with_warnings(ms(5), &mut messages)
        .unwrap();
    assert_eq!(pixel(&session, 1, 1), OVERSCAN);

    let place = |position| {
        format!(
            "{:?}",
            InputEvent::PointerPosition {
                port: PortId(2),
                position
            }
        )
    };
    let button = |pressed| {
        format!(
            "{:?}",
            InputEvent::PointerButton {
                port: PortId(2),
                button: 0,
                pressed
            }
        )
    };
    let centre = |pixel: u32, size: u32| (((2 * pixel + 1) << 16) / (2 * size)) as u16;
    assert_eq!(
        echoed(&messages),
        [
            // The click before any aiming: the gun's trigger, not a capture.
            button(true),
            button(false),
            place(Some((centre(10, 40), centre(7, 30)))),
            button(true),
            place(Some((centre(1, 40), centre(1, 30)))),
            button(false),
            place(None),
            place(Some((centre(1, 40), centre(1, 30)))),
            button(true),
            format!("{:?}", InputEvent::ReleaseAll),
            place(None),
        ]
    );
}

#[test]
fn a_reset_by_kind_goes_to_the_machine_and_restarts_a_halted_one() {
    let storage = MemoryStorage::new();
    let mut session = session_on(
        "unemu-test-core 1
clock 1000000
resets soft,hard
battery
at 5000 halt
",
        &storage,
    );
    assert_eq!(session.resets(), [ResetKind::Soft, ResetKind::Hard]);
    session.advance_fully(ms(3)).unwrap();
    assert_eq!(pixel(&session, 0, 0), background(3));
    session
        .reset(ResetKind::Soft, ms(3), &mut io::sink())
        .unwrap();
    session.advance_fully(ms(4)).unwrap();
    assert_eq!(
        pixel(&session, 0, 0),
        background(1),
        "the frames count again"
    );
    session.advance_fully(ms(10)).unwrap();
    assert!(session.awaits_reset());
    assert_eq!(session.machine().now(), Ticks(5_000));
    session
        .reset(ResetKind::Hard, ms(10), &mut io::sink())
        .unwrap();
    assert!(session.running && !session.halted);
    // Paced from the reset: the five milliseconds it waited halted are not
    // caught up.
    session.advance_fully(ms(11)).unwrap();
    assert_eq!(session.machine().now(), Ticks(6_000));
    session.finish(&mut io::sink()).unwrap();
    // The creation and the hard reset were starts; the soft one was not.
    let record = storage.durable(BATTERY_RECORD).unwrap();
    assert!(String::from_utf8(record).unwrap().ends_with("starts 2\n"));

    // A kind the machine does not list is refused and changes nothing.
    let mut session = self::session("unemu-test-core 1\nclock 1000000\nresets soft\n");
    assert_eq!(session.resets(), [ResetKind::Soft]);
    session.advance_fully(ms(3)).unwrap();
    let refused = session
        .reset(ResetKind::Hard, ms(3), &mut io::sink())
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert!(session.running);
    session.advance_fully(ms(4)).unwrap();
    assert_eq!(pixel(&session, 0, 0), background(4));
    // A stopped session has no machine to reset.
    session.running = false;
    let refused = session
        .reset(ResetKind::Soft, ms(4), &mut io::sink())
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Usage);
}

const STATES: &str = "unemu-test-core 1
clock 1000000
sound 48000 1000
pad 0 a,b
saved-state
";

#[test]
fn a_state_saved_changed_and_loaded_through_the_session() {
    let mut session = session(STATES);
    assert!(session.saves_state());
    session.audio.configure_for_test(48_000);
    session.audio.activate_for_test();
    session.key_event(KeyCode::KeyX, Pressed);
    session.advance_fully(ms(10)).unwrap();
    session.audio.drain_for_test();
    let saved = session.picture().unwrap().clone();
    let field = field_area(saved.visible);
    assert_eq!(
        pixel(&session, field.x, field.y),
        field_colour(&[&[PadControl::A]])
    );
    let identity = session.state_identity().unwrap();
    let mut state = Vec::new();
    session.save_state(&mut state).unwrap();

    // Change it: the other button, twenty more milliseconds, sound queued.
    session.key_event(KeyCode::KeyX, Released);
    session.key_event(KeyCode::KeyZ, Pressed);
    session.advance_fully(ms(30)).unwrap();
    assert_ne!(session.picture().unwrap().pixels, saved.pixels);
    assert!(session.audio.queued_for_test() > 0);

    session.load_state(&mut state.as_slice(), ms(30)).unwrap();
    assert_eq!(session.machine().now(), Ticks(10_000));
    assert_eq!(session.state_identity().as_deref(), Some(identity.as_str()));
    assert_eq!(
        session.picture().unwrap().pixels,
        saved.pixels,
        "the screen shows the loaded picture at once"
    );
    assert_eq!(
        session.audio.queued_for_test(),
        0,
        "the sound queued before the load is dropped"
    );

    // Paced from the load: ten milliseconds of host time are ten of guest
    // time from the loaded one, and the sound follows the loaded time.
    session.advance_fully(ms(40)).unwrap();
    assert_eq!(session.machine().now(), Ticks(20_000));
    let rate = Rate::new(48_000, 1);
    let tone = Rate::new(1_000, 1);
    let expected: Vec<_> = (480..960).map(|index| sample(index, rate, tone)).collect();
    assert_eq!(session.audio.drain_for_test(), expected);
    // The load released what the state held; Z, still down, presses afresh.
    assert_eq!(pixel(&session, field.x, field.y), field_colour(&[&[]]));
    session.key_event(KeyCode::KeyZ, Pressed);
    session.advance_fully(ms(42)).unwrap();
    assert_eq!(
        pixel(&session, field.x, field.y),
        field_colour(&[&[PadControl::B]])
    );
}

#[test]
fn a_state_of_another_identity_is_refused_and_the_session_goes_on() {
    let mut first = session(STATES);
    first.advance_fully(ms(5)).unwrap();
    let mut state = Vec::new();
    first.save_state(&mut state).unwrap();
    let mut other = session(&format!("{STATES}title another\n"));
    assert_ne!(other.state_identity(), first.state_identity());
    other.advance_fully(ms(3)).unwrap();
    let before = other.picture().unwrap().clone();
    let refused = other.load_state(&mut state.as_slice(), ms(3)).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::State);
    assert!(!refused.to_string().is_empty(), "a message to show");
    assert!(other.running);
    assert_eq!(other.machine().now(), Ticks(3_000));
    assert_eq!(other.picture(), Some(&before));
    other.advance_fully(ms(4)).unwrap();
    assert_eq!(other.machine().now(), Ticks(4_000), "pacing as before");
    // A state saved running runs; one saved halted waits for its reset.
    let mut halted = session(&format!("{STATES}at 2000 halt\n"));
    halted.advance_fully(ms(1)).unwrap();
    let mut running = Vec::new();
    halted.save_state(&mut running).unwrap();
    halted.advance_fully(ms(5)).unwrap();
    assert!(halted.awaits_reset());
    let mut waiting = Vec::new();
    halted.save_state(&mut waiting).unwrap();
    assert_eq!(halted.machine().status(), Status::Halted);
    halted.load_state(&mut running.as_slice(), ms(5)).unwrap();
    assert!(halted.running && !halted.awaits_reset());
    halted.load_state(&mut waiting.as_slice(), ms(5)).unwrap();
    assert!(halted.awaits_reset() && !halted.running);
}

#[test]
fn after_a_load_or_a_hard_reset_the_gun_points_where_the_host_pointer_rests() {
    let mut session = session(&format!("{GUN}saved-state\nresets soft,hard\n"));
    session.advance_fully(ms(1)).unwrap();
    let picture = session.picture().unwrap().clone();
    let layout = screen::layout(&picture, PANEL, false, false).unwrap();
    let place = |pixel| {
        let (x, y) = point_at(&layout, pixel);
        layout.pointer_position(x, y)
    };
    session.pointer_at(place((10, 7)));
    session.advance_fully(ms(2)).unwrap();
    let mut state = Vec::new();
    session.save_state(&mut state).unwrap();
    session.pointer_at(place((20, 15)));
    session.advance_fully(ms(3)).unwrap();
    assert_eq!(pixel(&session, 20, 15), cross_colour(0));

    // The state aimed elsewhere; the pointer rests and the trigger is pulled.
    session.load_state(&mut state.as_slice(), ms(3)).unwrap();
    assert_eq!(pixel(&session, 10, 7), cross_colour(0), "the saved picture");
    session.mouse_button(MouseButton::Left, Pressed);
    session.advance_fully(ms(5)).unwrap();
    assert_eq!(
        pixel(&session, 20, 15),
        cross_colour(1),
        "the shot is at the host place"
    );
    assert_eq!(pixel(&session, 10, 7), background(4));

    // A hard reset tells the device the place again, and nothing is held.
    session.mouse_button(MouseButton::Left, Released);
    let mut messages = Vec::new();
    session
        .advance_fully_with_warnings(ms(6), &mut messages)
        .unwrap();
    session
        .reset(ResetKind::Hard, ms(6), &mut io::sink())
        .unwrap();
    session
        .advance_fully_with_warnings(ms(7), &mut messages)
        .unwrap();
    let (x, y) = point_at(&layout, (20, 15));
    let again = format!(
        "{:?}",
        InputEvent::PointerPosition {
            port: PortId(2),
            position: layout.pointer_position(x, y)
        }
    );
    assert_eq!(echoed(&messages).last(), Some(&again));
}
