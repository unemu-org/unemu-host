// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs: deadlines and step budgets with exact virtual time, frame stops,
//! events at their times, input at run boundaries, and the picture it
//! shows.

use crate::support::{create, messages, picture, run_to};
use unemu_core_api::{
    DisplayId, ErrorKind, FrameArea, InputEvent, Key, KeyState, MessageLevel, PadControl, PortId,
    ResetKind, RunLimit, Status, Stop, Ticks,
};
use unemu_test_core::picture::{background, cross_colour, field_area, field_colour, OVERSCAN};

#[test]
fn deadlines_are_met_at_the_next_whole_step() {
    let mut machine = create("unemu-test-core 1\nstep 4\nframe 40\n");
    assert_eq!(run_to(machine.as_mut(), 10), Stop::Deadline);
    assert_eq!(
        machine.now(),
        Ticks(12),
        "passes the deadline by less than a step"
    );
    assert_eq!(machine.steps(), 3);
    assert_eq!(
        run_to(machine.as_mut(), 12),
        Stop::Deadline,
        "a deadline already met"
    );
    assert_eq!(machine.now(), Ticks(12));
    assert_eq!(
        machine.run(RunLimit::steps(10)).expect("a step budget"),
        Stop::Steps
    );
    assert_eq!((machine.now(), machine.steps()), (Ticks(40), 10));
    let both = RunLimit::until(Ticks(100)).with_steps(Some(20));
    assert_eq!(machine.run(both).unwrap(), Stop::Steps);
    assert_eq!(machine.now(), Ticks(80));
    let neither = RunLimit::until(Ticks(0)).with_until(None);
    assert_eq!(machine.run(neither).unwrap_err().kind, ErrorKind::Usage);
}

#[test]
fn frame_stops_come_at_every_frame_edge_once() {
    let mut machine = create("unemu-test-core 1\nframe 100\nframe-stops\n");
    let on_frame = RunLimit::until(Ticks(1_000)).with_stop_on_frame(true);
    for edge in [100, 200, 300] {
        assert_eq!(machine.run(on_frame).unwrap(), Stop::Frame(DisplayId(0)));
        assert_eq!(machine.now(), Ticks(edge));
        assert_eq!(picture(machine.as_mut()).at(0, 0), background(edge / 100));
    }
    run_to(machine.as_mut(), 350);
    assert_eq!(machine.run(on_frame).unwrap(), Stop::Frame(DisplayId(0)));
    assert_eq!(machine.now(), Ticks(400));
    let short = RunLimit::until(Ticks(450)).with_stop_on_frame(true);
    assert_eq!(machine.run(short).unwrap(), Stop::Deadline);
}

#[test]
fn events_happen_at_their_times_in_script_order() {
    let mut machine = create(
        "unemu-test-core 1
frame 100
resets soft
at 250 warning late
at 150 note first
at 150 guest-reset
at 300 halt
at 300 note after the halt
at 400 power-off
at 500 fail the guest crashed
",
    );
    assert_eq!(run_to(machine.as_mut(), 1_000), Stop::Halted);
    assert_eq!(machine.now(), Ticks(300));
    assert_eq!(machine.status(), Status::Halted);
    let texts: Vec<(MessageLevel, String)> = messages(machine.as_mut())
        .into_iter()
        .map(|m| (m.level, m.text))
        .collect();
    assert_eq!(
        texts,
        [
            (MessageLevel::Note, "first".to_owned()),
            (
                MessageLevel::Note,
                "the guest reset the machine at 150".to_owned()
            ),
            (MessageLevel::Warning, "late".to_owned()),
        ]
    );
    // The guest's reset at 150 restarted the frame count: two frames since.
    assert_eq!(picture(machine.as_mut()).at(0, 0), background(2));
    assert_eq!(
        run_to(machine.as_mut(), 1_000),
        Stop::Halted,
        "halted at once"
    );
    assert_eq!(machine.now(), Ticks(300));
    machine
        .reset(ResetKind::Soft)
        .expect("a halted machine resets");
    assert_eq!(run_to(machine.as_mut(), 1_000), Stop::PoweredOff);
    assert_eq!(machine.now(), Ticks(400));
    assert_eq!(messages(machine.as_mut())[0].text, "after the halt");
    assert_eq!(machine.status(), Status::Off);
    assert_eq!(
        machine.reset(ResetKind::Soft).unwrap_err().kind,
        ErrorKind::Usage
    );
    assert_eq!(run_to(machine.as_mut(), 1_000), Stop::PoweredOff);
}

#[test]
fn a_fail_event_fails_the_run_as_a_guest_error() {
    let mut machine = create("unemu-test-core 1\nat 70 fail the guest crashed\n");
    let error = machine.run(RunLimit::until(Ticks(100))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Guest);
    assert_eq!(error.message, "the guest crashed");
    assert_eq!(machine.now(), Ticks(70));
    assert_eq!(machine.status(), Status::Failed);
    machine.frame(DisplayId(0)).expect("the last picture stays");
}

const INPUT: &str = "unemu-test-core 1
picture 40 30
aspect 4 3
visible 4 2 32 26
frame 10
keyboard 0
pad 1 up,a,start
pad 2 b
pointer 3 2
absolute 4 1
echo-input
";

#[test]
fn input_takes_effect_at_the_next_run_and_shows_at_the_next_frame() {
    let mut machine = create(INPUT);
    let first = picture(machine.as_mut());
    assert_eq!((first.width, first.height, first.aspect), (40, 30, (4, 3)));
    assert_eq!(
        first.visible,
        FrameArea {
            x: 4,
            y: 2,
            width: 32,
            height: 26
        }
    );
    assert_eq!(first.at(0, 0), OVERSCAN);
    assert_eq!(first.at(39, 29), OVERSCAN);
    assert_eq!(first.at(4, 2), background(0));
    let field = field_area(first.visible);
    assert_eq!(first.at(field.x, field.y), field_colour(&[]));
    // The relative pointer starts at the centre.
    assert_eq!(first.at(20, 15), cross_colour(0));

    for event in [
        InputEvent::Pad {
            port: PortId(1),
            control: PadControl::A,
            pressed: true,
        },
        InputEvent::Pad {
            port: PortId(1),
            control: PadControl::Start,
            pressed: true,
        },
        // Not a control of this port: ignored.
        InputEvent::Pad {
            port: PortId(1),
            control: PadControl::B,
            pressed: true,
        },
        InputEvent::Pad {
            port: PortId(2),
            control: PadControl::B,
            pressed: true,
        },
        InputEvent::Key {
            port: PortId(0),
            key: Key::A,
            state: KeyState::Pressed,
        },
        InputEvent::PointerMotion {
            port: PortId(3),
            dx: 5,
            dy: 3,
        },
        InputEvent::PointerButton {
            port: PortId(3),
            button: 1,
            pressed: true,
        },
        InputEvent::PointerPosition {
            port: PortId(4),
            position: Some((0, 65535)),
        },
    ] {
        machine.input(event);
    }
    assert!(
        messages(machine.as_mut()).is_empty(),
        "nothing before the run"
    );
    run_to(machine.as_mut(), 5);
    assert_eq!(
        messages(machine.as_mut()).len(),
        8,
        "every event at the boundary"
    );
    assert_eq!(
        picture(machine.as_mut()).pixels,
        first.pixels,
        "no frame finished yet"
    );
    run_to(machine.as_mut(), 10);
    let shown = picture(machine.as_mut());
    assert!(shown.serial > first.serial);
    assert_eq!(
        shown.at(field.x, field.y),
        field_colour(&[&[PadControl::A, PadControl::Start], &[PadControl::B]])
    );
    assert_eq!(
        shown.at(25, 12),
        cross_colour(0b10),
        "moved right 5 and up 3"
    );
    assert_eq!(
        shown.at(0, 29),
        cross_colour(0),
        "the pen at the bottom-left pixel"
    );
    assert_eq!(shown.at(4, 2), background(1));

    // ReleaseAll lets go of every control and button and keeps the places.
    machine.input(InputEvent::ReleaseAll);
    run_to(machine.as_mut(), 20);
    let released = picture(machine.as_mut());
    assert_eq!(released.at(field.x, field.y), field_colour(&[]));
    assert_eq!(released.at(25, 12), cross_colour(0));
    assert_eq!(released.at(0, 29), cross_colour(0));
    let unchanged_serial = released.serial;
    assert_eq!(picture(machine.as_mut()).serial, unchanged_serial);
}

#[test]
fn a_fail_content_event_fails_the_run_as_changed_content() {
    let mut machine = create("unemu-test-core 1\nat 30 fail-content the cartridge changed\n");
    let error = machine.run(RunLimit::until(Ticks(100))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Content);
    assert_eq!(error.message, "the cartridge changed");
    assert_eq!(machine.now(), Ticks(30));
    assert_eq!(machine.status(), Status::Failed);
}

#[test]
fn limits_past_the_end_of_virtual_time_are_refused_and_change_nothing() {
    let mut machine = create("unemu-test-core 1\nstep 4\nframe 40\nframe-stops\n");
    run_to(machine.as_mut(), 8);
    for limit in [
        RunLimit::steps(u64::MAX),
        RunLimit::steps(u64::MAX / 4 + 1),
        RunLimit::until(Ticks(u64::MAX)),
        RunLimit::until(Ticks(u64::MAX - 2)).with_stop_on_frame(true),
    ] {
        let error = machine.run(limit).expect_err("past the end of time");
        assert_eq!(error.kind, ErrorKind::Usage, "{limit:?}");
        assert_eq!(machine.status(), Status::Ready, "{limit:?}");
        assert_eq!(machine.now(), Ticks(8), "{limit:?}");
    }
    // The last whole step of virtual time is reachable.
    let last = u64::MAX / 4;
    assert_eq!(machine.run(RunLimit::steps(last)).unwrap(), Stop::Steps);
    assert_eq!(machine.now(), Ticks(last * 4));
    // No frame finishes after it: a frame stop alone is refused.
    let error = machine
        .run(
            RunLimit::until(Ticks(0))
                .with_until(None)
                .with_stop_on_frame(true),
        )
        .expect_err("no frame left");
    assert_eq!(error.kind, ErrorKind::Usage);
    assert_eq!(machine.status(), Status::Ready);
    // An event past the end never comes; a deadline before it still works.
    let mut machine = create("unemu-test-core 1\nstep 4\nat 18446744073709551615 halt\n");
    assert_eq!(run_to(machine.as_mut(), 100), Stop::Deadline);
    assert_eq!(
        machine.run(RunLimit::steps(u64::MAX / 4)).unwrap(),
        Stop::Steps
    );
    assert_eq!(machine.status(), Status::Ready);
}

#[test]
fn a_third_pad_does_not_show_and_moves_no_serial() {
    let mut machine =
        create("unemu-test-core 1\nframe 10\npad 0 a\npad 1 a\npad 2 a\nresets soft\n");
    run_to(machine.as_mut(), 10);
    let before = picture(machine.as_mut());
    // A reset at the edge makes the next frame show the same count again,
    // so only the third pad's control differs between the two frames.
    machine.reset(ResetKind::Soft).expect("reset");
    machine.input(InputEvent::Pad {
        port: PortId(2),
        control: PadControl::A,
        pressed: true,
    });
    run_to(machine.as_mut(), 20);
    let after = picture(machine.as_mut());
    assert_eq!(after.pixels, before.pixels);
    assert_eq!(after.serial, before.serial, "the pixels did not change");
}
