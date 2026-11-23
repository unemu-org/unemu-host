// SPDX-License-Identifier: AGPL-3.0-or-later
//! The description is honest: no call that the core, platform or machine
//! description promises answers `Unsupported`, and the calls it does not
//! promise are refused as such.

use crate::support::{create, identify, CORE};
use unemu_core_api::test_support::MemoryStorage;
use unemu_core_api::{
    Content, ContractVersion, DisplayId, ErrorKind, InputEvent, InputKind, MediaAction, NewMedium,
    Outputs, ResetKind, RunLimit, SlotId, Ticks, WriteTarget, CONTRACT,
};

pub const EVERYTHING: &str = "unemu-test-core 1
title Everything
picture 32 24
aspect 4 3
visible 2 2 28 20
clock 48000
frame 800
sound 48000 1000
keyboard 0
pointer 1 3
absolute 2 1 Light pen
pad 3 up,down,left,right,a,b,start,select Controller 1
pad 4 a,b
slot 0 floppy removable utc-disk A:
medium 0 utc-disk 0101010101010101010101010101010101010101010101010101010101010101 1474560 first
slot 1 cartridge fixed utc-cart
resets soft,hard
saved-state
frame-stops
media-changes
battery
";

#[test]
fn the_core_states_contract_0_3_and_one_platform_that_opens_scripts() {
    let description = CORE.description();
    assert_eq!(description.implements, ContractVersion::new(0, 3));
    assert!(CONTRACT.drives(description.implements));
    assert_eq!(description.platforms.len(), 1);
    let platform = &description.platforms[0];
    assert!(!platform.config_types.is_empty());
    assert_eq!(platform.content.len(), 1);
    assert!(platform.content[0].is_valid());
    assert_eq!(platform.content[0].extensions, &["utc"]);
}

#[test]
fn no_promised_call_answers_unsupported() {
    let description = CORE.description();
    let platform = &description.platforms[0];
    // The platform promises identify and validate, because it lists content
    // kinds and config types.
    let identified = identify(EVERYTHING).expect("identify").expect("a script");
    CORE.validate(platform.id, identified.config.as_document())
        .expect("validate");
    let storage = MemoryStorage::new();
    let mut machine = crate::support::create_on(EVERYTHING, &storage).expect("create");
    let features = machine.description().features;
    // The machine may do at most what its platform may.
    assert!(features
        .resets
        .iter()
        .all(|kind| platform.features.resets.contains(kind)));
    assert!(!features.saved_state || platform.features.saved_state);
    assert!(!features.media_changes || platform.features.media_changes);
    assert!(!features.frame_stops || platform.features.frame_stops);
    let audio = machine.description().audio.expect("sound");
    assert!(audio.rate_in_bounds());

    // Every display has a picture, also before the first run.
    let displays: Vec<DisplayId> = machine
        .description()
        .displays
        .iter()
        .map(|d| d.id)
        .collect();
    for display in &displays {
        machine.frame(*display).expect("a promised display");
    }
    machine.set_outputs(Outputs::NONE.with_audio(true));
    for port in machine.description().inputs.clone() {
        match port.kind {
            InputKind::Keyboard => machine.input(InputEvent::Key {
                port: port.id,
                key: unemu_core_api::Key::A,
                state: unemu_core_api::KeyState::Pressed,
            }),
            InputKind::Pad { controls } => {
                for control in controls {
                    machine.input(InputEvent::Pad {
                        port: port.id,
                        control,
                        pressed: true,
                    });
                }
            }
            InputKind::RelativePointer { .. } => machine.input(InputEvent::PointerMotion {
                port: port.id,
                dx: 1,
                dy: 1,
            }),
            InputKind::AbsolutePointer { .. } => machine.input(InputEvent::PointerPosition {
                port: port.id,
                position: Some((100, 100)),
            }),
            _ => {}
        }
    }
    let stop = machine
        .run(RunLimit::until(Ticks(1000)).with_stop_on_frame(features.frame_stops))
        .expect("a frame stop is promised");
    assert_eq!(stop, unemu_core_api::Stop::Frame(DisplayId(0)));
    machine.run(RunLimit::steps(1600)).expect("a step budget");
    let mut sound = Vec::new();
    machine.drain_audio(&mut sound).expect("sound is promised");
    assert!(!sound.is_empty());
    for slot in machine.description().slots.clone() {
        let accepts = machine
            .slot_states()
            .into_iter()
            .find(|state| state.id == slot.id)
            .expect("every slot has a state")
            .accepts_changes;
        assert_eq!(accepts, slot.removable && features.media_changes);
        if accepts {
            let index = machine
                .add_medium(
                    slot.id,
                    NewMedium::new(
                        "second",
                        Content::Bytes(vec![0; 16]),
                        [2; 32],
                        16,
                        &slot.formats[0],
                        WriteTarget::Kept {
                            record: "kept-second",
                        },
                    ),
                )
                .expect("a removable slot takes a medium");
            machine
                .change_medium(slot.id, MediaAction::Insert(index))
                .expect("insert");
            machine
                .change_medium(slot.id, MediaAction::Next)
                .expect("next");
            machine
                .change_medium(slot.id, MediaAction::Eject)
                .expect("eject");
        }
    }
    machine
        .run(RunLimit::until(Ticks(4000)))
        .expect("the changes apply");
    for kind in features.resets {
        machine.reset(*kind).expect("a listed reset");
    }
    assert!(machine.state_identity().is_some());
    let mut state = Vec::new();
    machine
        .save_state(&mut state)
        .expect("saved state is promised");
    machine
        .load_state(&mut state.as_slice())
        .expect("and loading it");
    machine.finish().expect("finish");
}

#[test]
fn calls_the_description_does_not_promise_are_refused_as_unsupported() {
    let mut machine =
        create("unemu-test-core 1\nslot 0 floppy removable utc\nslot 1 cartridge fixed utc\n");
    let features = machine.description().features;
    assert!(features.resets.is_empty() && !features.saved_state && !features.frame_stops);
    let kind = |result: Result<(), unemu_core_api::Error>| result.expect_err("refused").kind;
    assert_eq!(
        kind(
            machine
                .run(RunLimit::until(Ticks(10)).with_stop_on_frame(true))
                .map(drop)
        ),
        ErrorKind::Unsupported
    );
    assert_eq!(kind(machine.reset(ResetKind::Soft)), ErrorKind::Unsupported);
    assert_eq!(kind(machine.reset(ResetKind::Hard)), ErrorKind::Unsupported);
    assert_eq!(machine.state_identity(), None);
    assert_eq!(
        kind(machine.save_state(&mut Vec::new())),
        ErrorKind::Unsupported
    );
    assert_eq!(
        kind(machine.load_state(&mut &[][..])),
        ErrorKind::Unsupported
    );
    assert_eq!(
        kind(machine.change_medium(SlotId(0), MediaAction::Eject)),
        ErrorKind::Unsupported
    );
    // None of these left the machine unable to run.
    machine.run(RunLimit::until(Ticks(10))).expect("still runs");
    assert!(machine.description().audio.is_none());
    machine
        .drain_audio(&mut Vec::new())
        .expect("no sound, nothing to drain");
}
