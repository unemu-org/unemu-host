// SPDX-License-Identifier: AGPL-3.0-or-later
//! A state saved, changed and loaded gives the saved counters and picture.

use crate::support::{create, create_on, picture, run_to};
use unemu_core_api::test_support::MemoryStorage;
use unemu_core_api::{ErrorKind, InputEvent, PadControl, PortId, ResetKind, Status, Stop, Ticks};
use unemu_test_core::picture::{background, field_area, field_colour};

const SCRIPT: &str = "unemu-test-core 1
picture 16 12
clock 1000
frame 100
pad 0 a,b,start
absolute 1 1
resets soft,hard
saved-state
battery
at 5000 halt
";

#[test]
fn a_state_saved_changed_and_loaded_gives_the_saved_counters_and_picture() {
    let mut machine = create(SCRIPT);
    machine.input(InputEvent::Pad {
        port: PortId(0),
        control: PadControl::A,
        pressed: true,
    });
    machine.input(InputEvent::PointerPosition {
        port: PortId(1),
        position: Some((32768, 32768)),
    });
    run_to(machine.as_mut(), 1_250);
    let saved_picture = picture(machine.as_mut());
    assert_eq!(saved_picture.at(0, 0), background(12));
    let field = field_area(saved_picture.visible);
    assert_eq!(
        saved_picture.at(field.x, field.y),
        field_colour(&[&[PadControl::A]])
    );
    let identity = machine.state_identity().expect("the machine saves state");
    let mut state = Vec::new();
    machine.save_state(&mut state).expect("save");

    // Change everything the counters hold.
    machine.input(InputEvent::Pad {
        port: PortId(0),
        control: PadControl::B,
        pressed: true,
    });
    machine.input(InputEvent::PointerPosition {
        port: PortId(1),
        position: None,
    });
    run_to(machine.as_mut(), 3_000);
    machine.reset(ResetKind::Hard).expect("a start");
    assert_eq!(run_to(machine.as_mut(), 6_000), Stop::Halted);
    let changed = picture(machine.as_mut());
    assert_ne!(changed.pixels, saved_picture.pixels);
    assert_eq!(machine.status(), Status::Halted);

    machine.load_state(&mut state.as_slice()).expect("load");
    assert_eq!(machine.now(), Ticks(1_250));
    assert_eq!(machine.steps(), 1_250);
    assert_eq!(machine.status(), Status::Ready);
    assert_eq!(machine.state_identity().as_deref(), Some(identity.as_str()));
    let loaded = picture(machine.as_mut());
    assert_eq!(loaded.pixels, saved_picture.pixels);
    assert!(
        loaded.serial > changed.serial,
        "a new picture gets a new serial"
    );
    // Saving again gives the same bytes: every counter came back.
    let mut again = Vec::new();
    machine.save_state(&mut again).expect("save");
    assert_eq!(again, state);
    // And the machine goes on from there: the halt at 5 000 comes again.
    assert_eq!(run_to(machine.as_mut(), 9_000), Stop::Halted);
    assert_eq!(machine.now(), Ticks(5_000));
}

#[test]
fn a_state_for_another_script_or_damaged_is_refused_and_changes_nothing() {
    let mut machine = create(SCRIPT);
    run_to(machine.as_mut(), 700);
    let mut state = Vec::new();
    machine.save_state(&mut state).expect("save");
    let mut other = create(&format!("{SCRIPT}at 6000 note later\n"));
    assert_ne!(other.state_identity(), machine.state_identity());
    run_to(other.as_mut(), 300);
    let before = picture(other.as_mut());
    let refused = other.load_state(&mut state.as_slice()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::State);
    let mut short = state.clone();
    short.pop();
    let refused = machine.load_state(&mut short.as_slice()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::State);
    assert_eq!(other.now(), Ticks(300));
    assert_eq!(picture(other.as_mut()), before);
    assert_eq!(machine.now(), Ticks(700));
}

#[test]
fn loaded_battery_memory_is_written_again() {
    let storage = MemoryStorage::new();
    let mut machine = create_on(SCRIPT, &storage).expect("create");
    let mut state = Vec::new();
    machine.save_state(&mut state).expect("save at one start");
    machine.reset(ResetKind::Hard).expect("two starts");
    machine.finish().expect("finish");
    drop(machine);
    let mut machine = create_on(SCRIPT, &storage).expect("three starts");
    machine
        .load_state(&mut state.as_slice())
        .expect("back to one");
    machine.finish().expect("finish");
    let durable = storage.durable(unemu_test_core::BATTERY_RECORD).unwrap();
    assert!(String::from_utf8(durable).unwrap().ends_with("starts 1\n"));
}

#[test]
fn loading_the_picture_already_shown_keeps_its_serial() {
    let mut machine = create(SCRIPT);
    run_to(machine.as_mut(), 1_250);
    let mut state = Vec::new();
    machine.save_state(&mut state).expect("save");
    let before = picture(machine.as_mut());
    run_to(machine.as_mut(), 1_299);
    machine.load_state(&mut state.as_slice()).expect("load");
    assert_eq!(
        picture(machine.as_mut()),
        before,
        "same pixels, same serial"
    );
}
