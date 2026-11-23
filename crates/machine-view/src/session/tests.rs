// SPDX-License-Identifier: AGPL-3.0-or-later
//! The session against a scripted machine: no platform, no host window.

use super::*;
use std::io::{Read, Write};
use unemu_core_api::{
    AudioDescription, DisplayDescription, Error, Features, Finished, Frame, FrameArea,
    InputPortDescription, Key, MachineDescription, MessageLevel, NewMedium, Outputs, PadControl,
    Rate, SlotDescription, SlotKind, SlotState, Status,
};
use winit::event::ElementState::{Pressed, Released};

/// A machine whose clock runs a million ticks a second and whose every run
/// reaches its deadline, unless a stop or a failure is scripted for it.
struct Scripted {
    description: MachineDescription,
    now: u64,
    status: Status,
    runs: Vec<RunLimit>,
    inputs: Vec<InputEvent>,
    changes: Vec<(SlotId, MediaAction)>,
    outputs: Vec<Outputs>,
    next: Vec<Result<Stop, Error>>,
    audio_failure: Option<ErrorKind>,
    messages: Vec<Message>,
    finish_warnings: Vec<String>,
    finished: u32,
    resets: u32,
    /// How many media changes had arrived at each reset.
    changes_at_reset: Vec<usize>,
    active: Vec<SlotId>,
    pixels: Vec<u32>,
    /// The picture's width and height; `pixels` holds that many.
    size: (u32, u32),
    /// The visible part the frame states, if any.
    visible: Option<FrameArea>,
    /// Drain a 1 kHz tone at the described rate, exactly `now × rate / clock`
    /// frames in all, instead of one fixed frame per drain.
    tone: bool,
    /// Frames of the tone handed over so far.
    handed: u64,
}

fn scripted() -> Box<Scripted> {
    Box::new(Scripted {
        description: MachineDescription::new(Rate::new(1_000_000, 1), "step")
            .with_displays(vec![DisplayDescription::new(DisplayId(0), "Screen")])
            .with_audio(Some(AudioDescription::new(Rate::new(48_000, 1))))
            .with_inputs(vec![
                InputPortDescription::new(PortId(3), InputKind::Keyboard, "Keyboard"),
                InputPortDescription::new(
                    PortId(4),
                    InputKind::RelativePointer { buttons: 3 },
                    "Mouse",
                ),
            ])
            .with_slots(vec![
                SlotDescription::new(SlotId(7), SlotKind::FixedDisk, "C:", false),
                SlotDescription::new(SlotId(8), SlotKind::Floppy, "A:", true),
                SlotDescription::new(SlotId(9), SlotKind::Floppy, "B:", true),
            ])
            .with_features(Features::NONE.with_resets(&[ResetKind::Soft])),
        now: 0,
        status: Status::Ready,
        runs: Vec::new(),
        inputs: Vec::new(),
        changes: Vec::new(),
        outputs: Vec::new(),
        next: Vec::new(),
        audio_failure: None,
        messages: vec![Message::new(MessageLevel::Warning, "created")],
        finish_warnings: Vec::new(),
        finished: 0,
        resets: 0,
        changes_at_reset: Vec::new(),
        active: Vec::new(),
        pixels: vec![0],
        size: (1, 1),
        visible: None,
        tone: false,
        handed: 0,
    })
}

impl Machine for Scripted {
    fn description(&self) -> &MachineDescription {
        &self.description
    }

    fn now(&self) -> Ticks {
        Ticks(self.now)
    }

    fn steps(&self) -> u64 {
        self.now
    }

    fn status(&self) -> Status {
        self.status
    }

    fn run(&mut self, limit: RunLimit) -> Result<Stop, Error> {
        self.runs.push(limit);
        if !self.next.is_empty() {
            let result = self.next.remove(0);
            self.status = match &result {
                Ok(Stop::Halted) => Status::Halted,
                Ok(Stop::PoweredOff) => Status::Off,
                Ok(_) => Status::Ready,
                Err(error) if error.kind == ErrorKind::Storage => self.status,
                Err(_) => Status::Failed,
            };
            return result;
        }
        self.now = limit.until.map_or(self.now, |until| until.0);
        Ok(Stop::Deadline)
    }

    fn frame(&mut self, _display: DisplayId) -> Result<Frame<'_>, Error> {
        self.pixels[0] = self.now as u32;
        let frame = Frame::new(self.size.0, self.size.1, &self.pixels, (4, 3), self.now);
        Ok(match self.visible {
            Some(visible) => frame.with_visible(visible),
            None => frame,
        })
    }

    fn drain_audio(&mut self, out: &mut Vec<[i16; 2]>) -> Result<(), Error> {
        if let Some(kind) = self.audio_failure {
            return Err(Error::new(kind, "no sound"));
        }
        if !self.tone {
            out.push([1, -1]);
            return Ok(());
        }
        let rate = self.description.audio.unwrap().rate;
        let clock = self.description.clock;
        let due = u128::from(self.now) * u128::from(rate.numerator) * u128::from(clock.denominator)
            / (u128::from(rate.denominator) * u128::from(clock.numerator));
        while u128::from(self.handed) < due {
            let seconds = self.handed as f64 * rate.denominator as f64 / rate.numerator as f64;
            let phase = std::f64::consts::TAU * 1000.0 * seconds;
            out.push([
                (12000.0 * phase.sin()).round() as i16,
                (9000.0 * phase.cos()).round() as i16,
            ]);
            self.handed += 1;
        }
        Ok(())
    }

    fn set_outputs(&mut self, outputs: Outputs) {
        self.outputs.push(outputs);
    }

    fn input(&mut self, event: InputEvent) {
        self.inputs.push(event);
    }

    fn slot_states(&mut self) -> Vec<SlotState> {
        let active = std::mem::take(&mut self.active);
        self.description
            .slots
            .iter()
            .map(|slot| SlotState::new(slot.id).with_active(active.contains(&slot.id)))
            .collect()
    }

    fn add_medium(&mut self, _slot: SlotId, _medium: NewMedium<'_>) -> Result<usize, Error> {
        Err(Error::new(ErrorKind::Unsupported, "no catalogue"))
    }

    fn change_medium(&mut self, slot: SlotId, action: MediaAction) -> Result<(), Error> {
        if slot == SlotId(9) {
            return Err(Error::new(ErrorKind::Usage, "drive B: is empty-handed"));
        }
        self.changes.push((slot, action));
        Ok(())
    }

    fn reset(&mut self, kind: ResetKind) -> Result<(), Error> {
        assert_eq!(kind, ResetKind::Soft);
        self.resets += 1;
        self.changes_at_reset.push(self.changes.len());
        self.status = Status::Ready;
        self.messages
            .push(Message::new(MessageLevel::Note, "reset"));
        Ok(())
    }

    fn state_identity(&self) -> Option<String> {
        None
    }

    fn save_state(&mut self, _out: &mut dyn Write) -> Result<(), Error> {
        Err(Error::new(ErrorKind::Unsupported, "no state"))
    }

    fn load_state(&mut self, _state: &mut dyn Read) -> Result<(), Error> {
        Err(Error::new(ErrorKind::Unsupported, "no state"))
    }

    fn finish(&mut self) -> Result<Finished, Error> {
        self.finished += 1;
        Ok(Finished::new().with_warnings(self.finish_warnings.clone()))
    }

    fn drain_messages(&mut self, out: &mut Vec<Message>) {
        out.append(&mut self.messages);
    }
}

fn session(machine: Box<Scripted>) -> Session<Scripted> {
    let mut warnings = Vec::new();
    let session = Session::new(machine, DisplayId(0), &mut warnings).unwrap();
    assert_eq!(warnings, b"created\n");
    session
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

#[test]
fn a_session_runs_one_millisecond_chunks_to_the_paced_target_and_keeps_the_picture() {
    let mut session = session(scripted());
    session.advance_fully(ms(10)).unwrap();
    let machine = session.machine();
    assert_eq!(machine.now, 10_000);
    assert_eq!(machine.runs.len(), 10);
    assert_eq!(
        machine.runs[3],
        RunLimit::until(Ticks(4_000)),
        "chunks end a millisecond of guest time apart"
    );
    // Sound is drained before the first chunk and after every chunk.
    assert_eq!(machine.outputs.len(), 11);
    let picture = session.picture().unwrap();
    assert_eq!(
        (picture.width, picture.height, picture.serial),
        (1, 1, 10_000)
    );
    assert_eq!(picture.pixels, [10_000]);
    assert_eq!(
        picture.visible,
        FrameArea {
            x: 0,
            y: 0,
            width: 1,
            height: 1
        },
        "a machine that states nothing has the whole picture visible"
    );
    assert!(session.running);
    // A paused (occluded) session neither runs nor drains.
    session.occlusion_changed(true, ms(10));
    session.advance_fully(ms(30)).unwrap();
    assert_eq!(session.machine().runs.len(), 10);
    session.occlusion_changed(false, ms(40));
    session.advance_fully(ms(41)).unwrap();
    assert_eq!(session.machine().now, 11_000, "hidden time is discarded");
}

#[test]
fn a_halted_machine_waits_for_its_reset_and_a_guest_error_keeps_the_last_picture() {
    let mut machine = scripted();
    machine.next.push(Ok(Stop::Halted));
    let mut session = session(machine);
    session.advance_fully(ms(5)).unwrap();
    assert!(!session.running && session.halted);
    assert!(session.awaits_reset() && session.takes_media());
    assert_eq!(
        session.machine().finished,
        0,
        "a halted session ends at Stop"
    );
    session.media_events.push((SlotId(8), MediaAction::Next));
    let mut diagnostics = Vec::new();
    session.reset_halted(&mut diagnostics).unwrap();
    assert_eq!(diagnostics, b"reset\n");
    assert_eq!(session.machine().resets, 1);
    assert_eq!(session.machine().changes, [(SlotId(8), MediaAction::Next)]);
    assert!(session.running && !session.halted);
    session
        .machine_mut()
        .next
        .push(Err(Error::new(ErrorKind::Guest, "triple fault")));
    session.advance_fully(ms(6)).unwrap();
    assert!(!session.running);
    assert_eq!(session.error.as_ref().unwrap().to_string(), "triple fault");
    assert!(session.picture().is_some());
    assert!(!session.takes_media() && !session.awaits_reset());
    assert_eq!(
        session.machine().finished,
        0,
        "a failed session is never sealed"
    );
}

#[test]
fn a_machine_switched_off_is_finished_with_its_warnings() {
    let mut machine = scripted();
    machine.next.push(Ok(Stop::PoweredOff));
    machine.finish_warnings = vec!["journal kept".into()];
    let mut session = session(machine);
    let mut warnings = Vec::new();
    session
        .advance_fully_with_warnings(ms(5), &mut warnings)
        .unwrap();
    assert!(!session.running && !session.halted);
    assert_eq!(session.machine().finished, 1);
    assert_eq!(warnings, b"warning: journal kept\n");
}

#[test]
fn the_picture_carries_the_visible_part_the_machine_states() {
    let mut machine = scripted();
    machine.size = (4, 2);
    machine.pixels = vec![0; 8];
    let visible = FrameArea {
        x: 1,
        y: 0,
        width: 2,
        height: 2,
    };
    machine.visible = Some(visible);
    let mut session = session(machine);
    session.advance_fully(ms(1)).unwrap();
    let picture = session.picture().unwrap();
    assert_eq!((picture.width, picture.height), (4, 2));
    assert_eq!(picture.pixels.len(), 8, "the whole picture is copied");
    assert_eq!(picture.visible, visible);
}

#[test]
fn storage_failures_are_returned_and_audio_failures_are_reported_once() {
    let mut machine = scripted();
    machine.audio_failure = Some(ErrorKind::Unsupported);
    let mut session = session(machine);
    let mut warnings = Vec::new();
    session
        .advance_fully_with_warnings(ms(3), &mut warnings)
        .unwrap();
    session
        .advance_fully_with_warnings(ms(4), &mut warnings)
        .unwrap();
    assert_eq!(
        warnings,
        b"audio observation failed: no sound; continuing\n"
    );
    session.machine_mut().audio_failure = Some(ErrorKind::Storage);
    let error = session.advance_fully(ms(5)).unwrap_err();
    assert_eq!(error.to_string(), "no sound");
    session.machine_mut().audio_failure = Some(ErrorKind::StorageUncertain);
    let error = session
        .advance_fully_with_warnings(ms(5), &mut warnings)
        .unwrap_err();
    assert_eq!(error.to_string(), "no sound");
    session.machine_mut().audio_failure = None;
    session
        .machine_mut()
        .next
        .push(Err(Error::new(ErrorKind::Storage, "disk full")));
    assert_eq!(
        session.advance_fully(ms(6)).unwrap_err().to_string(),
        "disk full"
    );
    assert!(session.running && session.error.is_none());
}

#[test]
fn media_keys_change_the_first_and_second_removable_slot_and_never_reach_the_guest() {
    let mut session = session(scripted());
    for (key, state) in [
        (KeyCode::ControlLeft, Pressed),
        (KeyCode::AltLeft, Pressed),
        (KeyCode::KeyF, Pressed),
        (KeyCode::KeyF, Pressed),
        (KeyCode::KeyF, Released),
        (KeyCode::ShiftLeft, Pressed),
        (KeyCode::KeyE, Pressed),
        (KeyCode::KeyE, Released),
    ] {
        session.key_event(key, state);
    }
    assert_eq!(
        session.media_events,
        [
            (SlotId(8), MediaAction::Next),
            (SlotId(9), MediaAction::Eject)
        ]
    );
    let keys: Vec<_> = session
        .machine()
        .inputs
        .iter()
        .map(|event| match event {
            InputEvent::Key { port, key, state } => (*port, key.0, *state),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        keys,
        [
            (PortId(3), 0xE0, KeyState::Pressed),
            (PortId(3), 0xE2, KeyState::Pressed),
            (PortId(3), 0xE1, KeyState::Pressed),
        ],
        "the guest sees the modifiers only"
    );
    // A slot that cannot change now is skipped.
    session.apply_media_events().unwrap();
    assert_eq!(session.machine().changes, [(SlotId(8), MediaAction::Next)]);
}

#[test]
fn focus_loss_releases_everything_once_and_capture_forwards_counts_and_buttons() {
    let mut session = session(scripted());
    session.mouse.pointer_inside(true);
    session.mouse_button_with_grab(MouseButton::Left, Pressed, || Ok(()));
    session.mouse_button_with_grab(MouseButton::Left, Released, || Ok(()));
    assert!(session.mouse.captured());
    session.mouse_motion(1.5, -2.25);
    session.mouse_motion(0.5, -0.75);
    session.mouse_button_with_grab(MouseButton::Right, Pressed, || Ok(()));
    session.key_event(KeyCode::KeyA, Pressed);
    session.focus_changed(false);
    assert!(!session.mouse.captured());
    let pointer = PortId(4);
    assert_eq!(
        session.machine().inputs,
        [
            InputEvent::PointerMotion {
                port: pointer,
                dx: 1,
                dy: 2
            },
            InputEvent::PointerMotion {
                port: pointer,
                dx: 1,
                dy: 1
            },
            InputEvent::PointerButton {
                port: pointer,
                button: 1,
                pressed: true
            },
            InputEvent::Key {
                port: PortId(3),
                key: unemu_core_api::Key::A,
                state: KeyState::Pressed
            },
            InputEvent::ReleaseAll,
        ]
    );
    // A stopped session takes no input; its capture release still lifts the
    // guest's buttons.
    session.focus_changed(true);
    session.mouse.pointer_inside(true);
    session.mouse_button_with_grab(MouseButton::Left, Pressed, || Ok(()));
    session.mouse_button_with_grab(MouseButton::Left, Released, || Ok(()));
    session.mouse_button_with_grab(MouseButton::Middle, Pressed, || Ok(()));
    session.running = false;
    session.key_event(KeyCode::KeyB, Pressed);
    session.release_pointer();
    let inputs = &session.machine().inputs;
    assert_eq!(
        inputs[inputs.len() - 2..],
        [
            InputEvent::PointerButton {
                port: pointer,
                button: 2,
                pressed: true
            },
            InputEvent::PointerButton {
                port: pointer,
                button: 2,
                pressed: false
            },
        ]
    );
}

#[test]
fn the_drive_lights_take_each_busy_slot_once() {
    let mut session = session(scripted());
    session.machine_mut().active = vec![SlotId(7), SlotId(9)];
    assert_eq!(session.take_slot_activity(), [SlotId(7), SlotId(9)]);
    assert!(session.take_slot_activity().is_empty());
}

#[test]
fn sound_at_a_rate_the_window_does_not_play_disables_audio_with_one_warning() {
    let mut machine = scripted();
    machine.description = machine
        .description
        .clone()
        .with_audio(Some(AudioDescription::new(Rate::new(384_001, 2))));
    let mut session = session(machine);
    let mut warnings = Vec::new();
    session.audio.open(&mut warnings);
    session.audio.open(&mut warnings);
    session
        .advance_fully_with_warnings(ms(2), &mut warnings)
        .unwrap();
    assert_eq!(
        String::from_utf8(warnings).unwrap(),
        "audio disabled: the machine's sound runs at 384001/2 Hz; the window plays 8000 to 192000 Hz\n"
    );
    assert!(!session.audio.available());
    assert!(session
        .machine()
        .outputs
        .iter()
        .all(|outputs| !outputs.audio));
}

/// Ten virtual seconds of a scripted machine's tone at `rate`, through the
/// session to a device at 48 kHz, taken out of the ring after every advance.
fn played_through_a_session(rate: Rate) -> Vec<[i16; 2]> {
    let mut machine = scripted();
    machine.description = machine
        .description
        .clone()
        .with_audio(Some(AudioDescription::new(rate)));
    machine.tone = true;
    let mut session = session(machine);
    assert_eq!(session.audio.source(), rate);
    session.audio.configure_for_test(48_000);
    session.audio.activate_for_test();
    let mut played = Vec::new();
    for millis in (10..=10_000).step_by(10) {
        session.advance_fully(ms(millis)).unwrap();
        played.extend(session.audio.drain_for_test());
    }
    assert_eq!(session.machine().now, 10_000_000);
    played
}

#[test]
fn sound_at_any_rate_the_contract_allows_reaches_the_device_whole() {
    for rate in [Rate::new(984_375, 22), Rate::new(44_100, 1)] {
        let played = played_through_a_session(rate);
        // The machine handed over now × rate / clock frames; the converter
        // played every place up to the newest one, at 48 kHz.
        let (n, d) = (u128::from(rate.numerator), u128::from(rate.denominator));
        let handed = 10 * n / d;
        assert_eq!(
            played.len() as u128,
            (handed - 1) * 48_000 * d / n + 1,
            "{rate:?}"
        );
        let rising = played
            .windows(2)
            .filter(|pair| pair[0][0] < 0 && pair[1][0] >= 0)
            .count();
        let periods = 1000.0 * played.len() as f64 / 48_000.0;
        assert!((rising as f64 - periods).abs() <= 1.0, "{rate:?}: {rising}");
        let omega_t = std::f64::consts::TAU * 1000.0 * d as f64 / n as f64;
        let tolerance = 12000.0 * omega_t * omega_t / 8.0 + 2.0;
        for (index, frame) in played.iter().enumerate() {
            let phase = std::f64::consts::TAU * 1000.0 * index as f64 / 48_000.0;
            assert!(
                (f64::from(frame[0]) - 12000.0 * phase.sin()).abs() <= tolerance,
                "{rate:?}: frame {index}"
            );
        }
    }
}

/// Two pads, listed out of port order, and no keyboard.
fn two_pads() -> Box<Scripted> {
    let mut machine = scripted();
    let all = vec![
        PadControl::Up,
        PadControl::Down,
        PadControl::Left,
        PadControl::Right,
        PadControl::A,
        PadControl::B,
        PadControl::Start,
        PadControl::Select,
    ];
    machine.description = machine.description.clone().with_inputs(vec![
        InputPortDescription::new(
            PortId(6),
            InputKind::Pad {
                controls: all.clone(),
            },
            "Controller 2",
        ),
        InputPortDescription::new(
            PortId(2),
            InputKind::Pad {
                controls: all[..6].to_vec(),
            },
            "Controller 1",
        ),
    ]);
    machine
}

/// The default pad keys with `extra` bound on top.
fn bound(extra: &[(usize, PadControl, Key)]) -> PadBindings {
    let mut bindings = PadBindings::defaults();
    for &(player, control, key) in extra {
        bindings.bind(player, control, key);
    }
    bindings
}

fn pad(port: u8, control: PadControl, pressed: bool) -> InputEvent {
    InputEvent::Pad {
        port: PortId(port),
        control,
        pressed,
    }
}

#[test]
fn bound_keys_work_the_pads_of_a_machine_without_a_keyboard_and_others_reach_nobody() {
    let mut session = session(two_pads());
    session.set_pad_bindings(bound(&[(1, PadControl::Start, Key::DIGIT_1)]));
    for (key, state) in [
        (KeyCode::KeyX, Pressed),
        (KeyCode::KeyX, Pressed),
        (KeyCode::ArrowLeft, Pressed),
        (KeyCode::KeyX, Released),
        // Unbound: nobody gets it.
        (KeyCode::KeyK, Pressed),
        (KeyCode::KeyK, Released),
        // Player 0's Start: its port lists no Start.
        (KeyCode::Enter, Pressed),
        (KeyCode::Enter, Released),
        // Player 1 is the second port in port order.
        (KeyCode::Digit1, Pressed),
        (KeyCode::Digit1, Released),
    ] {
        session.key_event(key, state);
    }
    session.focus_changed(false);
    assert_eq!(
        session.machine().inputs,
        [
            pad(2, PadControl::A, true),
            pad(2, PadControl::Left, true),
            pad(2, PadControl::A, false),
            pad(6, PadControl::Start, true),
            pad(6, PadControl::Start, false),
            InputEvent::ReleaseAll,
        ]
    );
}

#[test]
fn a_pad_control_with_two_keys_stays_held_until_both_are_up_and_chords_stay_the_hosts() {
    let mut session = session(two_pads());
    session.set_pad_bindings(bound(&[
        (0, PadControl::A, Key::SPACE),
        (0, PadControl::Up, Key::M),
    ]));
    session.audio.set_active_for_test(true);
    for (key, state) in [
        (KeyCode::KeyX, Pressed),
        (KeyCode::Space, Pressed),
        (KeyCode::KeyX, Released),
        (KeyCode::Space, Released),
        (KeyCode::ControlLeft, Pressed),
        (KeyCode::AltLeft, Pressed),
        (KeyCode::KeyM, Pressed),
        (KeyCode::KeyM, Released),
    ] {
        session.key_event(key, state);
    }
    assert!(session.audio.muted(), "the mute chord is the host's");
    assert_eq!(
        session.machine().inputs,
        [pad(2, PadControl::A, true), pad(2, PadControl::A, false)]
    );
    // Focus loss forgets held keys: the next press is fresh.
    session.key_event(KeyCode::KeyZ, Pressed);
    session.focus_changed(false);
    session.focus_changed(true);
    session.key_event(KeyCode::KeyZ, Pressed);
    let inputs = &session.machine().inputs;
    assert_eq!(
        inputs[2..],
        [
            pad(2, PadControl::B, true),
            InputEvent::ReleaseAll,
            pad(2, PadControl::B, true),
        ]
    );
}

/// A machine with a keyboard, the PC's case, gets the bound keys as keys,
/// even when it also has a pad port.
#[test]
fn a_machine_with_a_keyboard_gets_bound_keys_as_todays_key_events() {
    let mut machine = scripted();
    let mut inputs = machine.description.inputs.clone();
    inputs.push(InputPortDescription::new(
        PortId(5),
        InputKind::Pad {
            controls: vec![PadControl::A, PadControl::Start],
        },
        "Controller",
    ));
    machine.description = machine.description.clone().with_inputs(inputs);
    for machine in [scripted(), machine] {
        let mut session = session(machine);
        for key in [
            KeyCode::ArrowUp,
            KeyCode::KeyX,
            KeyCode::KeyZ,
            KeyCode::Enter,
            KeyCode::ShiftRight,
        ] {
            session.key_event(key, Pressed);
            session.key_event(key, Pressed);
            session.key_event(key, Released);
        }
        let keys: Vec<_> = session
            .machine()
            .inputs
            .iter()
            .map(|event| match event {
                InputEvent::Key { port, key, state } => (*port, key.0, *state),
                other => panic!("{other:?}"),
            })
            .collect();
        let mut expected = Vec::new();
        for usage in [0x52, 0x1B, 0x1D, 0x28, 0xE5] {
            expected.extend([
                (PortId(3), usage, KeyState::Pressed),
                (PortId(3), usage, KeyState::Pressed),
                (PortId(3), usage, KeyState::Released),
            ]);
        }
        assert_eq!(keys, expected);
    }
}

#[test]
fn new_pad_bindings_release_the_held_controls_and_the_next_press_is_fresh() {
    let mut session = session(two_pads());
    session.key_event(KeyCode::KeyX, Pressed);
    session.key_event(KeyCode::Space, Pressed);
    // X now works B: the A it held is released at once.
    session.set_pad_bindings(bound(&[(0, PadControl::B, Key::X)]));
    assert_eq!(
        session.pad_bindings().control(Key::X),
        Some((0, PadControl::B))
    );
    session.key_event(KeyCode::KeyX, Released);
    session.key_event(KeyCode::KeyX, Pressed);
    session.key_event(KeyCode::KeyX, Released);
    assert_eq!(
        session.machine().inputs,
        [
            pad(2, PadControl::A, true),
            pad(2, PadControl::A, false),
            pad(2, PadControl::B, true),
            pad(2, PadControl::B, false),
        ]
    );
}

#[test]
fn a_reset_forgets_the_pad_keys_held_when_the_machine_halted() {
    let mut machine = two_pads();
    machine.next.push(Ok(Stop::Halted));
    let mut session = session(machine);
    session.key_event(KeyCode::KeyX, Pressed);
    session.advance_fully(ms(1)).unwrap();
    assert!(session.awaits_reset());
    // The key comes up while the machine is halted: nobody hears it.
    session.key_event(KeyCode::KeyX, Released);
    session.reset_halted(&mut Vec::new()).unwrap();
    session.key_event(KeyCode::KeyX, Pressed);
    assert_eq!(
        session.machine().inputs,
        [pad(2, PadControl::A, true), pad(2, PadControl::A, true)],
        "the press after the reset is not a repeat"
    );
}

#[test]
fn a_reset_puts_the_queued_media_changes_in_first() {
    let mut session = session(scripted());
    session.media_events.push((SlotId(8), MediaAction::Next));
    session
        .reset(ResetKind::Soft, ms(0), &mut Vec::new())
        .unwrap();
    assert!(session.media_events.is_empty());
    assert_eq!(session.machine().changes, [(SlotId(8), MediaAction::Next)]);
    assert_eq!(session.machine().changes_at_reset, [1]);
}
