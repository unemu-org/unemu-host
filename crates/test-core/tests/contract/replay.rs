// SPDX-License-Identifier: AGPL-3.0-or-later
//! The replay rule: the same creation and the same calls give the same
//! pictures, sound, messages, stops and records.

use crate::support::{create_on, drain, messages, picture, Picture};
use unemu_core_api::test_support::MemoryStorage;
use unemu_core_api::{
    InputEvent, Machine, MediaAction, Message, Outputs, PadControl, PortId, ResetKind, RunLimit,
    SlotId, Stop, Ticks,
};

const SCRIPT: &str = "unemu-test-core 1
picture 24 16
visible 1 1 22 14
clock 44100
frame 735
sound 44100 441
pad 0 a,b,start
pointer 1 1
slot 0 floppy removable utc
medium 0 utc 0202020202020202020202020202020202020202020202020202020202020202 10 one
medium 0 utc 0303030303030303030303030303030303030303030303030303030303030303 10 two
resets soft,hard
saved-state
frame-stops
media-changes
battery
echo-input
at 30000 note half way
at 441000 guest-reset
";

/// Everything a front end sees of a session.
#[derive(Debug, PartialEq, Eq)]
struct Session {
    stops: Vec<(Stop, Ticks, u64)>,
    pictures: Vec<Picture>,
    sound: Vec<[i16; 2]>,
    messages: Vec<Message>,
    record: Option<Vec<u8>>,
}

fn session(script: &str) -> Session {
    let storage = MemoryStorage::new();
    let mut machine = create_on(script, &storage).expect("create");
    let mut out = Session {
        stops: Vec::new(),
        pictures: Vec::new(),
        sound: Vec::new(),
        messages: Vec::new(),
        record: None,
    };
    let mut state = Vec::new();
    let record = |machine: &mut dyn Machine, stop: Stop, out: &mut Session| {
        out.stops.push((stop, machine.now(), machine.steps()));
        out.pictures.push(picture(machine));
        out.sound.extend(drain(machine));
        out.messages.extend(messages(machine));
    };
    for round in 0..40u64 {
        machine.input(InputEvent::Pad {
            port: PortId(0),
            control: if round % 3 == 0 {
                PadControl::A
            } else {
                PadControl::Start
            },
            pressed: round % 2 == 0,
        });
        machine.input(InputEvent::PointerMotion {
            port: PortId(1),
            dx: (round % 5) as i32 - 2,
            dy: (round % 7) as i32 - 3,
        });
        if round % 9 == 4 {
            machine
                .change_medium(SlotId(0), MediaAction::Next)
                .expect("next medium");
        }
        let limit = if round % 4 == 0 {
            RunLimit::until(Ticks(round * 13_001)).with_stop_on_frame(true)
        } else {
            RunLimit::until(Ticks(round * 13_001))
        };
        let stop = machine.run(limit).expect("run");
        record(machine.as_mut(), stop, &mut out);
        match round {
            10 => machine.set_outputs(Outputs::NONE),
            14 => machine.set_outputs(Outputs::NONE.with_audio(true)),
            20 => machine.save_state(&mut state).expect("save"),
            25 => machine.reset(ResetKind::Hard).expect("reset"),
            30 => machine.load_state(&mut state.as_slice()).expect("load"),
            _ => {}
        }
    }
    let stop = machine.run(RunLimit::steps(2_000_000)).expect("run");
    record(machine.as_mut(), stop, &mut out);
    machine.finish().expect("finish");
    out.record = storage.contents(unemu_test_core::BATTERY_RECORD);
    out
}

#[test]
fn the_same_script_and_calls_give_the_same_session() {
    let first = session(SCRIPT);
    let second = session(SCRIPT);
    assert!(first.sound.len() > 400_000, "the session had sound");
    assert!(first.messages.len() > 40, "and messages");
    assert!(first.record.is_some());
    assert_eq!(first, second);
    // A different script gives a different session: the comparison sees.
    let other = session(&SCRIPT.replace("sound 44100 441", "sound 44100 882"));
    assert_ne!(first.sound, other.sound);
}
