// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lifecycle and validation regressions for the contract's call/status matrix.

use crate::support::{create, create_on, drain, messages, picture, run_to};
use std::io::{self, Read};
use unemu_core_api::test_support::{MemoryStorage, Operation};
use unemu_core_api::*;
use unemu_test_core::picture::{cross_colour, field_area, field_colour};

const FULL: &str = "unemu-test-core 1
picture 40 30
frame 10
keyboard 0
pad 1 a
pointer 2 2
absolute 3 1
slot 0 floppy removable utc
media-changes
saved-state
resets soft,hard
echo-input
";

fn medium(writes: WriteTarget<'_>) -> NewMedium<'_> {
    NewMedium::new(
        "disk",
        Content::Bytes(vec![0; 8]),
        [3; 32],
        8,
        "utc",
        writes,
    )
}

fn saved(machine: &mut dyn Machine) -> Vec<u8> {
    let mut bytes = Vec::new();
    machine.save_state(&mut bytes).unwrap();
    bytes
}

#[test]
fn finished_and_failed_reject_mutations_before_capability_checks() {
    for features in ["", "saved-state\nresets soft,hard\nmedia-changes\n"] {
        for failed in [false, true] {
            let script = format!("unemu-test-core 1\n{features}at 1 fail stopped\n");
            let mut machine = create(&script);
            let expected = if failed {
                assert_eq!(
                    machine.run(RunLimit::steps(1)).unwrap_err().kind,
                    ErrorKind::Guest
                );
                Status::Failed
            } else {
                machine.finish().unwrap();
                Status::Finished
            };
            assert_eq!(machine.status(), expected);
            let before = picture(machine.as_mut());
            let now = machine.now();
            let steps = machine.steps();
            assert_eq!(machine.finish().unwrap_err().kind, ErrorKind::Usage);
            assert_eq!(
                machine.run(RunLimit::steps(2)).unwrap_err().kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine.input(InputEvent::ReleaseAll).unwrap_err().kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine.reset(ResetKind::Soft).unwrap_err().kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine.reset(ResetKind::Hard).unwrap_err().kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine
                    .change_medium(SlotId(99), MediaAction::Next)
                    .unwrap_err()
                    .kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine
                    .add_medium(SlotId(99), medium(WriteTarget::ReadOnly))
                    .unwrap_err()
                    .kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine.save_state(&mut Vec::new()).unwrap_err().kind,
                ErrorKind::Usage
            );
            assert_eq!(
                machine.load_state(&mut &[][..]).unwrap_err().kind,
                ErrorKind::Usage
            );
            machine.set_outputs(Outputs::NONE);
            drain(machine.as_mut());
            messages(machine.as_mut());
            machine.slot_states();
            machine.state_identity();
            assert_eq!(machine.status(), expected);
            assert_eq!((machine.now(), machine.steps()), (now, steps));
            assert_eq!(picture(machine.as_mut()), before);
        }
    }
}

#[test]
fn terminal_output_preferences_are_ignored_and_audio_remains_drainable() {
    for failed in [false, true] {
        let mut machine =
            create("unemu-test-core 1\nclock 8000\nsound 8000 100\nat 4 fail stopped\n");
        if failed {
            machine.run(RunLimit::steps(4)).unwrap_err();
        } else {
            run_to(machine.as_mut(), 4 - 1);
            machine.finish().unwrap();
        }
        machine.set_outputs(Outputs::NONE);
        assert_eq!(drain(machine.as_mut()).len(), if failed { 4 } else { 3 });
        assert!(drain(machine.as_mut()).is_empty());
    }
}

#[test]
fn invalid_input_is_refused_without_altering_the_queue() {
    let mut machine = create(FULL);
    let valid = InputEvent::key(PortId(0), Key::A, KeyState::Pressed);
    machine.input(valid.clone()).unwrap();
    let before = saved(machine.as_mut());
    for event in [
        InputEvent::key(PortId(99), Key::A, KeyState::Pressed),
        InputEvent::key(PortId(1), Key::A, KeyState::Pressed),
        InputEvent::pad(PortId(1), PadControl::B, true),
        InputEvent::pointer_motion(PortId(3), 1, 1),
        InputEvent::pointer_position(PortId(2), None),
        InputEvent::pointer_button(PortId(2), 2, true),
        InputEvent::pointer_button(PortId(3), 1, true),
    ] {
        assert_eq!(machine.input(event).unwrap_err().kind, ErrorKind::Usage);
        assert_eq!(saved(machine.as_mut()), before);
    }
    assert_eq!(machine.run(RunLimit::steps(0)).unwrap(), Stop::Steps);
    let echoed = messages(machine.as_mut());
    assert_eq!(echoed.len(), 1);
    assert!(echoed[0].text.ends_with(&format!("{valid:?}")));
    assert_eq!(machine.steps(), 0);
}

#[test]
fn unsupported_run_preserves_input_and_stopped_runs_short_circuit_validation() {
    for (event, status, stop) in [
        ("halt", Status::Halted, Stop::Halted),
        ("power-off", Status::Off, Stop::PoweredOff),
    ] {
        let mut machine = create(&format!("{FULL}at 1 {event}\n"));
        machine.input(InputEvent::ReleaseAll).unwrap();
        let before = saved(machine.as_mut());
        assert_eq!(
            machine
                .run(RunLimit::steps(2).with_stop_on_frame(true))
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
        assert_eq!(saved(machine.as_mut()), before);
        assert_eq!(machine.run(RunLimit::steps(2)).unwrap(), stop);
        if status == Status::Halted {
            machine.input(InputEvent::ReleaseAll).unwrap();
        }
        let before = saved(machine.as_mut());
        assert_eq!(
            machine
                .run(RunLimit::steps(u64::MAX).with_stop_on_frame(true))
                .unwrap(),
            stop
        );
        assert_eq!(
            machine
                .run(RunLimit::until(Ticks(0)).with_until(None))
                .unwrap(),
            stop
        );
        assert_eq!(saved(machine.as_mut()), before);
        assert_eq!(machine.status(), status);
    }
}

#[test]
fn off_refuses_input_media_and_soft_reset_but_hard_reset_and_state_io_work() {
    let mut machine = create(&format!("{FULL}at 1 power-off\n"));
    run_to(machine.as_mut(), 2);
    let before = saved(machine.as_mut());
    assert_eq!(
        machine.input(InputEvent::ReleaseAll).unwrap_err().kind,
        ErrorKind::Usage
    );
    assert_eq!(
        machine
            .change_medium(SlotId(99), MediaAction::Eject)
            .unwrap_err()
            .kind,
        ErrorKind::Usage
    );
    assert_eq!(
        machine
            .add_medium(SlotId(99), medium(WriteTarget::ReadOnly))
            .unwrap_err()
            .kind,
        ErrorKind::Usage
    );
    assert_eq!(
        machine.reset(ResetKind::Soft).unwrap_err().kind,
        ErrorKind::Usage
    );
    assert!(!machine.slot_states()[0].accepts_changes);
    assert_eq!(saved(machine.as_mut()), before);
    machine.load_state(&mut before.as_slice()).unwrap();
    assert_eq!(machine.status(), Status::Off);
    machine.reset(ResetKind::Hard).unwrap();
    assert_eq!(machine.status(), Status::Ready);
    assert_eq!((machine.now(), machine.steps()), (Ticks(1), 1));
    let mut machine = create("unemu-test-core 1\nat 1 power-off\n");
    run_to(machine.as_mut(), 1);
    assert_eq!(
        machine.reset(ResetKind::Soft).unwrap_err().kind,
        ErrorKind::Usage
    );
    assert_eq!(
        machine.reset(ResetKind::Hard).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    machine.finish().unwrap();
    assert_eq!(machine.status(), Status::Finished);
}

#[test]
fn unbounded_runs_end_at_scripted_events_or_counter_exhaustion() {
    let unbounded = RunLimit::until(Ticks(0)).with_until(None);
    for (event, stop) in [("halt", Stop::Halted), ("power-off", Stop::PoweredOff)] {
        let mut machine = create(&format!("unemu-test-core 1\nat 7 {event}\n"));
        assert_eq!(machine.run(unbounded).unwrap(), stop);
        assert_eq!(machine.steps(), 7);
    }
    let mut machine = create("unemu-test-core 1\nat 7 fail stopped\n");
    assert_eq!(machine.run(unbounded).unwrap_err().kind, ErrorKind::Guest);
    assert_eq!(machine.steps(), 7);
    let mut machine = create("unemu-test-core 1\nstep 4\n");
    assert_eq!(
        machine.run(RunLimit::steps(u64::MAX)).unwrap_err().kind,
        ErrorKind::Guest
    );
    assert_eq!(machine.status(), Status::Failed);
    assert_eq!(machine.now(), Ticks(u64::MAX / 4 * 4));
}

#[test]
fn reached_limits_return_normally_before_counter_exhaustion() {
    let mut machine = create("unemu-test-core 1\nstep 4\nframe-stops\n");
    let last = u64::MAX / 4;
    assert_eq!(machine.run(RunLimit::steps(last)).unwrap(), Stop::Steps);
    assert_eq!(
        machine
            .run(RunLimit::steps(last).with_stop_on_frame(true))
            .unwrap(),
        Stop::Steps
    );
    assert_eq!(machine.status(), Status::Ready);
    assert_eq!(
        machine
            .run(RunLimit::until(Ticks(u64::MAX)))
            .unwrap_err()
            .kind,
        ErrorKind::Guest
    );
    assert_eq!(machine.status(), Status::Failed);
}

#[test]
fn simultaneous_stops_follow_contract_priority() {
    for (events, expected) in [
        ("at 10 halt\nat 10 power-off\n", Ok(Stop::PoweredOff)),
        ("at 10 power-off\nat 10 halt\n", Ok(Stop::PoweredOff)),
        ("at 10 halt\nat 10 fail stopped\n", Err(ErrorKind::Guest)),
        (
            "at 10 power-off\nat 10 fail-content changed\n",
            Err(ErrorKind::Content),
        ),
        ("at 10 halt\n", Ok(Stop::Halted)),
        ("", Ok(Stop::Frame(DisplayId(0)))),
    ] {
        let mut machine = create(&format!(
            "unemu-test-core 1\nframe 10\nframe-stops\n{events}"
        ));
        let result = machine.run(
            RunLimit::until(Ticks(10))
                .with_steps(Some(10))
                .with_stop_on_frame(true),
        );
        assert_eq!(result.map_err(|e| e.kind), expected);
    }
    let mut machine = create("unemu-test-core 1\nframe 10\nframe-stops\n");
    assert_eq!(
        machine
            .run(RunLimit::until(Ticks(10)).with_steps(Some(10)))
            .unwrap(),
        Stop::Deadline
    );
}

#[test]
fn resets_clear_queued_and_held_controls_but_keep_time_positions_and_media() {
    for kind in [ResetKind::Soft, ResetKind::Hard] {
        let mut machine = create(FULL);
        let index = machine
            .add_medium(SlotId(0), medium(WriteTarget::ReadOnly))
            .unwrap();
        machine
            .change_medium(SlotId(0), MediaAction::Insert(index))
            .unwrap();
        for event in [
            InputEvent::pad(PortId(1), PadControl::A, true),
            InputEvent::key(PortId(0), Key::A, KeyState::Pressed),
            InputEvent::pointer_position(PortId(3), Some((0, 0))),
            InputEvent::pointer_button(PortId(3), 0, true),
        ] {
            machine.input(event).unwrap();
        }
        run_to(machine.as_mut(), 10);
        messages(machine.as_mut());
        machine
            .input(InputEvent::pad(PortId(1), PadControl::A, true))
            .unwrap();
        machine.reset(kind).unwrap();
        assert_eq!(
            (machine.now(), machine.steps(), machine.status()),
            (Ticks(10), 10, Status::Ready)
        );
        assert_eq!(machine.slot_states()[0].inserted, Some(index));
        run_to(machine.as_mut(), 20);
        assert!(
            messages(machine.as_mut()).is_empty(),
            "reset discards queued input"
        );
        let frame = picture(machine.as_mut());
        let field = field_area(frame.visible);
        assert_eq!(frame.at(field.x, field.y), field_colour(&[]));
        assert_eq!(
            frame.at(0, 0),
            cross_colour(0),
            "absolute position survives without held buttons"
        );
        let cleared = saved(machine.as_mut());
        let mut reference = create(FULL);
        let index = reference
            .add_medium(SlotId(0), medium(WriteTarget::ReadOnly))
            .unwrap();
        reference
            .change_medium(SlotId(0), MediaAction::Insert(index))
            .unwrap();
        reference
            .input(InputEvent::pointer_position(PortId(3), Some((0, 0))))
            .unwrap();
        run_to(reference.as_mut(), 10);
        reference.reset(kind).unwrap();
        run_to(reference.as_mut(), 20);
        assert_eq!(
            cleared,
            saved(reference.as_mut()),
            "keyboard and pointer buttons were cleared too"
        );
    }
}

#[test]
fn reset_refusal_preserves_the_queue_and_counters() {
    let mut machine = create("unemu-test-core 1\nkeyboard 0\nsaved-state\necho-input\n");
    machine
        .input(InputEvent::key(PortId(0), Key::A, KeyState::Pressed))
        .unwrap();
    let before = saved(machine.as_mut());
    assert_eq!(
        machine.reset(ResetKind::Hard).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(saved(machine.as_mut()), before);
    run_to(machine.as_mut(), 0);
    assert_eq!(messages(machine.as_mut()).len(), 1);
}

#[test]
fn media_capabilities_precede_invalid_arguments() {
    let mut machine = create("unemu-test-core 1\n");
    assert!(!machine.description().features.media_additions);
    assert_eq!(
        machine
            .add_medium(SlotId(99), medium(WriteTarget::ReadOnly))
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        machine
            .change_medium(SlotId(99), MediaAction::Next)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    let mut machine = create(FULL);
    assert!(machine.description().features.media_additions);
    assert_eq!(
        machine
            .add_medium(SlotId(99), medium(WriteTarget::ReadOnly))
            .unwrap_err()
            .kind,
        ErrorKind::Usage
    );
}

#[test]
fn media_changes_are_immediate_in_halted_and_reuse_compatible_kept_aliases() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(&format!("{FULL}at 1 halt\n"), &storage).unwrap();
    run_to(machine.as_mut(), 2);
    let first = machine
        .add_medium(SlotId(0), medium(WriteTarget::kept("kept-disk")))
        .unwrap();
    let alias = machine
        .add_medium(SlotId(0), medium(WriteTarget::kept("kept-disk")))
        .unwrap();
    machine
        .change_medium(SlotId(0), MediaAction::Insert(first))
        .unwrap();
    assert!(storage.is_open("kept-disk"));
    machine
        .change_medium(SlotId(0), MediaAction::Insert(alias))
        .unwrap();
    assert_eq!(machine.slot_states()[0].inserted, Some(alias));
    assert_eq!(machine.status(), Status::Halted);
    machine
        .change_medium(SlotId(0), MediaAction::Eject)
        .unwrap();
    assert!(!storage.is_open("kept-disk"));
    assert_eq!(machine.slot_states()[0].inserted, None);
}

#[test]
fn failed_media_read_preserves_insertion_and_uncertainty_fails_the_machine() {
    for kind in [ErrorKind::Storage, ErrorKind::StorageUncertain] {
        let storage = MemoryStorage::new();
        storage.insert("kept-new", b"external bytes");
        let mut machine = create_on(FULL, &storage).unwrap();
        let old = machine
            .add_medium(SlotId(0), medium(WriteTarget::ReadOnly))
            .unwrap();
        let new = machine
            .add_medium(SlotId(0), medium(WriteTarget::kept("kept-new")))
            .unwrap();
        machine
            .change_medium(SlotId(0), MediaAction::Insert(old))
            .unwrap();
        let token = machine.state_identity();
        storage.fail_next("kept-new", Operation::Read, Error::new(kind, "read failed"));
        assert_eq!(
            machine
                .change_medium(SlotId(0), MediaAction::Insert(new))
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(machine.slot_states()[0].inserted, Some(old));
        assert_eq!(machine.state_identity(), token);
        assert_eq!(
            machine.status(),
            if kind == ErrorKind::Storage {
                Status::Ready
            } else {
                Status::Failed
            }
        );
        assert!(!storage.is_open("kept-new"));
    }
}

#[test]
fn saved_input_queue_is_restored_and_failed_reads_are_atomic_storage_errors() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }
    let mut machine = create(FULL);
    let queued = [
        InputEvent::key(PortId(0), Key::A, KeyState::Pressed),
        InputEvent::key(PortId(0), Key::A, KeyState::Repeated),
        InputEvent::key(PortId(0), Key::A, KeyState::Released),
        InputEvent::pad(PortId(1), PadControl::A, true),
        InputEvent::pointer_motion(PortId(2), -3, 4),
        InputEvent::pointer_button(PortId(2), 1, true),
        InputEvent::pointer_position(PortId(3), Some((123, 456))),
        InputEvent::pointer_position(PortId(3), None),
        InputEvent::ReleaseAll,
    ];
    for event in &queued {
        machine.input(event.clone()).unwrap();
    }
    let state = saved(machine.as_mut());
    run_to(machine.as_mut(), 20);
    messages(machine.as_mut());
    machine.input(InputEvent::ReleaseAll).unwrap();
    let before = saved(machine.as_mut());
    assert_eq!(
        machine.load_state(&mut Broken).unwrap_err().kind,
        ErrorKind::Storage
    );
    assert_eq!(saved(machine.as_mut()), before);
    machine.load_state(&mut state.as_slice()).unwrap();
    assert_eq!(saved(machine.as_mut()), state);
    run_to(machine.as_mut(), 0);
    let echoed = messages(machine.as_mut());
    assert_eq!(echoed.len(), queued.len());
    for (message, event) in echoed.iter().zip(&queued) {
        assert_eq!(message.text, format!("input at 0: {event:?}"));
    }
}

#[test]
fn state_identity_covers_catalogue_and_external_record_generation() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(FULL, &storage).unwrap();
    let empty = saved(machine.as_mut());
    let index = machine
        .add_medium(SlotId(0), medium(WriteTarget::kept("kept-disk")))
        .unwrap();
    assert_eq!(
        machine.load_state(&mut empty.as_slice()).unwrap_err().kind,
        ErrorKind::State
    );
    machine
        .change_medium(SlotId(0), MediaAction::Insert(index))
        .unwrap();
    let state = saved(machine.as_mut());
    machine
        .change_medium(SlotId(0), MediaAction::Eject)
        .unwrap();
    storage.insert("kept-disk", b"changed generation");
    machine
        .change_medium(SlotId(0), MediaAction::Insert(index))
        .unwrap();
    let before = saved(machine.as_mut());
    assert_eq!(
        machine.load_state(&mut state.as_slice()).unwrap_err().kind,
        ErrorKind::State
    );
    assert_eq!(saved(machine.as_mut()), before);
    assert_eq!(
        storage.contents("kept-disk").unwrap(),
        b"changed generation"
    );
}
