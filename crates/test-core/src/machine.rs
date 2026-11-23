// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scripted machine behind [`crate::TestCore::create`].

use crate::config::hex;
use crate::picture::{self, control_bit, Shown};
use crate::script::{EventKind, PortKind, Script};
use crate::sound;
use crate::state::{self, Counters, Held, Shape};
use std::io::{Read, Write};
use std::rc::Rc;
use unemu_core_api::{
    is_kept_record_name, pointer_pixel, AudioDescription, DisplayDescription, DisplayId, Error,
    ErrorKind, Features, Finished, Frame, InputEvent, InputKind, InputPortDescription, KeyState,
    Machine, MachineDescription, MediaAction, MediumInfo, Message, MessageLevel, NewMedium,
    Outputs, PortId, Record, ResetKind, RunLimit, SlotDescription, SlotId, SlotState, Status, Stop,
    Storage, WriteKind, WriteTarget,
};

/// The record that holds the battery-backed memory: the machine's starts.
pub const BATTERY_RECORD: &str = "battery";

const BATTERY_FIRST_LINE: &str = "unemu-test-core battery 1";

/// Virtual seconds after which changed battery memory is written at the end
/// of a run.
const BATTERY_INTERVAL_SECONDS: u128 = 10;

/// Where an input port's events go.
#[derive(Clone, Copy, Debug)]
enum Role {
    Pad {
        index: usize,
        allowed: u16,
    },
    Keyboard {
        index: usize,
    },
    Pointer {
        index: usize,
        relative: bool,
        buttons: u8,
    },
}

/// The battery record and what was last written to it.
struct Battery {
    /// Held for the machine's life. A failed write fails the machine, so a
    /// record left needing to be opened again is never written again.
    record: Box<dyn Record>,
    written_starts: u64,
    written_at: u64,
}

/// One slot as it runs.
struct SlotRun {
    description: SlotDescription,
    catalogue: Vec<MediumInfo>,
    /// The record each entry's writes are kept in, for media added with
    /// [`WriteTarget::Kept`].
    kept: Vec<Option<String>>,
    inserted: Option<usize>,
    record: Option<Box<dyn Record>>,
    active: bool,
    generation: Vec<u8>,
}

/// A machine that runs a script.
pub(crate) struct TestMachine {
    description: MachineDescription,
    script: Script,
    roles: Vec<(PortId, Role)>,
    identity: String,
    storage: Rc<dyn Storage>,
    battery: Option<Battery>,
    counters: Counters,
    failed: bool,
    finished: bool,
    pending_input: Vec<InputEvent>,
    slots: Vec<SlotRun>,
    audio_on: bool,
    audio_next: u64,
    messages: Vec<Message>,
    pixels: Vec<u32>,
    serial: u64,
    drawn: Option<u64>,
}

fn storage_failure(error: Error) -> Error {
    match error.kind {
        ErrorKind::Storage | ErrorKind::StorageUncertain => error,
        _ => Error::new(ErrorKind::Storage, error.message),
    }
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn unsupported(message: &str) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

/// The starts a battery record holds; an empty record holds none.
fn read_starts(record: &mut dyn Record) -> Result<u64, Error> {
    let size = record.size()?;
    if size == 0 {
        return Ok(0);
    }
    let foreign = || {
        Error::new(
            ErrorKind::Storage,
            "the battery record was not written by the test core",
        )
    };
    if size > 64 {
        return Err(foreign());
    }
    let mut bytes = vec![0; size as usize];
    record.read_at(0, &mut bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| foreign())?;
    let mut lines = text.lines();
    if lines.next() != Some(BATTERY_FIRST_LINE) {
        return Err(foreign());
    }
    lines
        .next()
        .and_then(|line| line.strip_prefix("starts "))
        .and_then(|count| count.parse().ok())
        .ok_or_else(foreign)
}

/// The pad masks a frame shows: the first two pad ports'.
fn drawn_pads(held: &[u16]) -> Vec<u16> {
    held.iter().copied().take(2).collect()
}

fn battery_bytes(starts: u64) -> Vec<u8> {
    format!("{BATTERY_FIRST_LINE}\nstarts {starts}\n").into_bytes()
}

impl TestMachine {
    pub(crate) fn new(
        script: Script,
        script_sha256: &[u8; 32],
        storage: Rc<dyn Storage>,
    ) -> Result<Self, Error> {
        let features = Features::NONE
            .with_resets(script.resets)
            .with_saved_state(script.saved_state)
            .with_media_changes(script.media_changes)
            .with_media_additions(script.media_changes)
            .with_frame_stops(script.frame_stops);
        let mut roles = Vec::new();
        let mut inputs = Vec::new();
        let (mut pads, mut keyboards, mut pointers) = (0, 0, Vec::new());
        let centre = (script.width / 2, script.height / 2);
        for port in &script.ports {
            let (role, kind) = match &port.kind {
                PortKind::Keyboard => {
                    keyboards += 1;
                    (
                        Role::Keyboard {
                            index: keyboards - 1,
                        },
                        InputKind::Keyboard,
                    )
                }
                PortKind::Pointer { buttons } => {
                    pointers.push((Some(centre), 0));
                    (
                        Role::Pointer {
                            index: pointers.len() - 1,
                            relative: true,
                            buttons: *buttons,
                        },
                        InputKind::relative_pointer(*buttons),
                    )
                }
                PortKind::Absolute { buttons } => {
                    pointers.push((None, 0));
                    (
                        Role::Pointer {
                            index: pointers.len() - 1,
                            relative: false,
                            buttons: *buttons,
                        },
                        InputKind::absolute_pointer(DisplayId(0), *buttons),
                    )
                }
                PortKind::Pad { controls } => {
                    pads += 1;
                    let allowed = controls.iter().fold(0, |mask, &c| mask | control_bit(c));
                    (
                        Role::Pad {
                            index: pads - 1,
                            allowed,
                        },
                        InputKind::pad(controls.clone()),
                    )
                }
            };
            roles.push((port.id, role));
            inputs.push(InputPortDescription::new(port.id, kind, port.label.clone()));
        }
        let slots: Vec<SlotRun> = script
            .slots
            .iter()
            .map(|slot| SlotRun {
                description: SlotDescription::new(
                    slot.id,
                    slot.kind,
                    slot.label.clone(),
                    slot.removable,
                )
                .with_formats(slot.formats.clone()),
                catalogue: slot.catalogue.clone(),
                kept: vec![None; slot.catalogue.len()],
                inserted: slot.inserted,
                record: None,
                active: false,
                generation: Vec::new(),
            })
            .collect();
        let description = MachineDescription::new(script.clock, "step")
            .with_displays(vec![DisplayDescription::new(DisplayId(0), "Screen")])
            .with_audio(script.sound.map(|sound| AudioDescription::new(sound.rate)))
            .with_inputs(inputs)
            .with_slots(slots.iter().map(|slot| slot.description.clone()).collect())
            .with_features(features);
        let (battery, starts) = if script.battery {
            let mut record = storage.open(BATTERY_RECORD)?;
            let written = read_starts(record.as_mut())?;
            (
                Some(Battery {
                    record,
                    written_starts: written,
                    written_at: 0,
                }),
                written.saturating_add(1),
            )
        } else {
            (None, 0)
        };
        let held = Held {
            pads: vec![0; pads],
            keys: vec![Vec::new(); keyboards],
            pointers,
        };
        let counters = Counters {
            now: 0,
            started_at: 0,
            starts,
            events_done: 0,
            status: Status::Ready,
            shown: Shown {
                frames: 0,
                pads: drawn_pads(&held.pads),
                pointers: held.pointers.clone(),
            },
            held,
        };
        Ok(Self {
            description,
            identity: format!(
                "unemu-test-core {} script {}",
                env!("CARGO_PKG_VERSION"),
                hex(script_sha256)
            ),
            script,
            roles,
            storage,
            battery,
            counters,
            failed: false,
            finished: false,
            pending_input: Vec::new(),
            slots,
            audio_on: true,
            audio_next: 0,
            messages: Vec::new(),
            pixels: Vec::new(),
            serial: 1,
            drawn: None,
        })
    }

    fn fail(&mut self, error: Error) -> Error {
        self.failed = true;
        error
    }

    fn live(&self) -> Result<(), Error> {
        if self.failed {
            Err(usage("the machine failed"))
        } else if self.finished {
            Err(usage("the session is finished"))
        } else {
            Ok(())
        }
    }

    fn interactive(&self) -> Result<(), Error> {
        self.live()?;
        if self.counters.status == Status::Off {
            Err(usage("the machine is switched off"))
        } else {
            Ok(())
        }
    }

    fn validate_input(&self, event: &InputEvent) -> Result<(), Error> {
        let valid =
            match event {
                InputEvent::ReleaseAll => true,
                InputEvent::Key { port, .. }
                | InputEvent::PointerMotion { port, .. }
                | InputEvent::PointerPosition { port, .. }
                | InputEvent::PointerButton { port, .. }
                | InputEvent::Pad { port, .. } => self.description.inputs.iter().any(|input| {
                    input.id == *port
                        && match (event, &input.kind) {
                            (InputEvent::Key { .. }, InputKind::Keyboard)
                            | (
                                InputEvent::PointerMotion { .. },
                                InputKind::RelativePointer { .. },
                            )
                            | (
                                InputEvent::PointerPosition { .. },
                                InputKind::AbsolutePointer { .. },
                            ) => true,
                            (
                                InputEvent::PointerButton { button, .. },
                                InputKind::RelativePointer { buttons, .. }
                                | InputKind::AbsolutePointer { buttons, .. },
                            ) => button < buttons,
                            (InputEvent::Pad { control, .. }, InputKind::Pad { controls, .. }) => {
                                controls.contains(control)
                            }
                            _ => false,
                        }
                }),
                _ => return Err(unsupported("unknown input operation")),
            };
        if !valid {
            return Err(usage("invalid input port or control"));
        }
        Ok(())
    }

    fn shape(&self) -> Shape {
        Shape {
            pads: self.counters.held.pads.len(),
            shown_pads: self.counters.shown.pads.len(),
            keyboards: self.counters.held.keys.len(),
            pointers: self.counters.held.pointers.len(),
            events: self.script.events.len(),
            width: self.script.width,
            height: self.script.height,
        }
    }

    fn audio_target(&self) -> u64 {
        match self.script.sound {
            Some(sound) => sound::frame_index(
                unemu_core_api::Ticks(self.counters.now),
                self.script.clock,
                sound.rate,
            ),
            None => 0,
        }
    }

    /// The first boundary at or after `ticks`, saturated past the end of
    /// virtual time.
    fn boundary(&self, ticks: u64) -> u128 {
        u128::from(ticks).div_ceil(u128::from(self.script.step)) * u128::from(self.script.step)
    }

    /// Advance to `end`, drawing the last frame that finishes on the way.
    fn advance_to(&mut self, end: u64) {
        let frame = self.script.frame;
        let now = self.counters.now;
        if end / frame > now / frame {
            let last_edge = end / frame * frame;
            let frames = last_edge / frame - self.counters.started_at / frame;
            let shown = Shown {
                frames,
                pads: drawn_pads(&self.counters.held.pads),
                pointers: self.counters.held.pointers.clone(),
            };
            if shown != self.counters.shown {
                self.counters.shown = shown;
                self.serial += 1;
            }
        }
        self.counters.now = end;
    }

    fn apply_input(&mut self, event: InputEvent) {
        if self.script.echo_input {
            self.messages.push(Message::new(
                MessageLevel::Note,
                format!("input at {}: {event:?}", self.counters.now),
            ));
        }
        let role = |port: PortId| {
            self.roles
                .iter()
                .find(|(id, _)| *id == port)
                .map(|(_, role)| *role)
        };
        let held = &mut self.counters.held;
        match event {
            InputEvent::Pad {
                port,
                control,
                pressed,
                ..
            } => {
                if let Some(Role::Pad { index, allowed }) = role(port) {
                    let bit = control_bit(control) & allowed;
                    if pressed {
                        held.pads[index] |= bit;
                    } else {
                        held.pads[index] &= !bit;
                    }
                }
            }
            InputEvent::Key {
                port, key, state, ..
            } => {
                if let Some(Role::Keyboard { index }) = role(port) {
                    let keys = &mut held.keys[index];
                    match (state, keys.binary_search(&key.0)) {
                        (KeyState::Pressed, Err(at)) => keys.insert(at, key.0),
                        (KeyState::Released, Ok(at)) => {
                            keys.remove(at);
                        }
                        _ => {}
                    }
                }
            }
            InputEvent::PointerMotion { port, dx, dy, .. } => {
                if let Some(Role::Pointer {
                    index,
                    relative: true,
                    ..
                }) = role(port)
                {
                    let (x, y) = held.pointers[index].0.unwrap_or((0, 0));
                    let column =
                        (i64::from(x) + i64::from(dx)).clamp(0, i64::from(self.script.width) - 1);
                    let row =
                        (i64::from(y) - i64::from(dy)).clamp(0, i64::from(self.script.height) - 1);
                    held.pointers[index].0 = Some((column as u32, row as u32));
                }
            }
            InputEvent::PointerButton {
                port,
                button,
                pressed,
                ..
            } => {
                if let Some(Role::Pointer { index, buttons, .. }) = role(port) {
                    if button < buttons {
                        let bit = 1u8 << button;
                        if pressed {
                            held.pointers[index].1 |= bit;
                        } else {
                            held.pointers[index].1 &= !bit;
                        }
                    }
                }
            }
            InputEvent::PointerPosition { port, position, .. } => {
                if let Some(Role::Pointer {
                    index,
                    relative: false,
                    ..
                }) = role(port)
                {
                    held.pointers[index].0 = position
                        .map(|place| pointer_pixel(place, self.script.width, self.script.height));
                }
            }
            InputEvent::ReleaseAll => {
                held.pads.fill(0);
                held.keys.iter_mut().for_each(Vec::clear);
                held.pointers.iter_mut().for_each(|pointer| pointer.1 = 0);
            }
            _ => {}
        }
    }

    /// Write the battery memory; a failed write fails the machine.
    fn write_battery(&mut self) -> Result<(), Error> {
        let Some(battery) = self.battery.as_mut() else {
            return Ok(());
        };
        let starts = self.counters.starts;
        let result = battery.record.replace().and_then(|mut replacement| {
            replacement.append(&battery_bytes(starts))?;
            replacement.commit()
        });
        match result {
            Ok(()) => {
                battery.written_starts = starts;
                battery.written_at = self.counters.now;
                Ok(())
            }
            Err(error) => Err(self.fail(storage_failure(error))),
        }
    }

    /// Write changed battery memory once ten virtual seconds have passed
    /// since the last write.
    fn battery_checkpoint(&mut self) -> Result<(), Error> {
        let Some(battery) = &self.battery else {
            return Ok(());
        };
        let clock = self.script.clock;
        let gone = u128::from(self.counters.now.saturating_sub(battery.written_at));
        let due = gone * u128::from(clock.denominator)
            >= BATTERY_INTERVAL_SECONDS * u128::from(clock.numerator);
        if battery.written_starts != self.counters.starts && due {
            self.write_battery()?;
        }
        Ok(())
    }

    fn restart(&mut self) {
        self.counters.started_at = self.counters.now;
        self.counters.status = Status::Ready;
    }

    fn run_loop(&mut self, limit: RunLimit) -> Result<Stop, Error> {
        let start = self.counters.now;
        let step = self.script.step;
        let frame = self.script.frame;
        loop {
            let now = u128::from(self.counters.now);
            // Limits are absolute; an already reached limit performs no work.
            if limit
                .until
                .is_some_and(|until| self.counters.now >= until.0)
            {
                return Ok(Stop::Deadline);
            }
            if limit.steps.is_some_and(|steps| self.steps() >= steps) {
                return Ok(Stop::Steps);
            }
            let mut candidates = Vec::with_capacity(4);
            if let Some(until) = limit.until {
                candidates.push(self.boundary(until.0).max(now));
            }
            if let Some(steps) = limit.steps {
                candidates.push((u128::from(steps) * u128::from(step)).max(now));
            }
            if limit.stop_on_frame {
                candidates.push((now / u128::from(frame) + 1) * u128::from(frame));
            }
            if let Some(event) = self.script.events.get(self.counters.events_done) {
                candidates.push(self.boundary(event.at).max(now));
            }
            let Some(end) = candidates
                .into_iter()
                .filter_map(|ticks| u64::try_from(ticks).ok())
                .min()
            else {
                let last = u64::MAX / step * step;
                self.advance_to(last);
                return Err(self.fail(Error::new(ErrorKind::Guest, "virtual counter exhausted")));
            };
            self.advance_to(end);
            while let Some(event) = self.script.events.get(self.counters.events_done) {
                if self.boundary(event.at) > u128::from(end) {
                    break;
                }
                self.counters.events_done += 1;
                let what = event.what.clone();
                match what {
                    EventKind::Note(text) => {
                        self.messages.push(Message::new(MessageLevel::Note, text));
                    }
                    EventKind::Warning(text) => {
                        self.messages
                            .push(Message::new(MessageLevel::Warning, text));
                    }
                    EventKind::GuestReset => {
                        self.restart();
                        self.messages.push(Message::new(
                            MessageLevel::Note,
                            format!("the guest reset the machine at {end}"),
                        ));
                    }
                    EventKind::Halt | EventKind::PowerOff => {
                        // Script order still defers events after a stop until reset,
                        // but simultaneous terminal causes follow contract priority.
                        let mut chosen = what;
                        let mut priority = if matches!(chosen, EventKind::PowerOff) {
                            2
                        } else {
                            1
                        };
                        let mut consumed = self.counters.events_done;
                        for (index, event) in self.script.events.iter().enumerate().skip(consumed) {
                            if self.boundary(event.at) > u128::from(end) {
                                break;
                            }
                            let rank = match event.what {
                                EventKind::Fail(_) | EventKind::FailContent(_) => 3,
                                EventKind::PowerOff => 2,
                                EventKind::Halt => 1,
                                _ => continue,
                            };
                            consumed = index + 1;
                            if rank > priority {
                                chosen = event.what.clone();
                                priority = rank;
                            }
                        }
                        self.counters.events_done = consumed;
                        match chosen {
                            EventKind::Fail(text) => {
                                return Err(self.fail(Error::new(ErrorKind::Guest, text)))
                            }
                            EventKind::FailContent(text) => {
                                return Err(self.fail(Error::new(ErrorKind::Content, text)))
                            }
                            EventKind::PowerOff => {
                                self.counters.status = Status::Off;
                                return Ok(Stop::PoweredOff);
                            }
                            _ => {
                                self.counters.status = Status::Halted;
                                return Ok(Stop::Halted);
                            }
                        }
                    }
                    EventKind::Fail(text) => {
                        return Err(self.fail(Error::new(ErrorKind::Guest, text)));
                    }
                    EventKind::FailContent(text) => {
                        return Err(self.fail(Error::new(ErrorKind::Content, text)));
                    }
                }
            }
            if limit.stop_on_frame && end > start && end % frame == 0 {
                return Ok(Stop::Frame(DisplayId(0)));
            }
            if limit.until.is_some_and(|until| end >= until.0) {
                return Ok(Stop::Deadline);
            }
            if limit.steps.is_some_and(|steps| end / step >= steps) {
                return Ok(Stop::Steps);
            }
        }
    }
}

impl Machine for TestMachine {
    fn description(&self) -> &MachineDescription {
        &self.description
    }

    fn now(&self) -> unemu_core_api::Ticks {
        unemu_core_api::Ticks(self.counters.now)
    }

    fn steps(&self) -> u64 {
        self.counters.now / self.script.step
    }

    fn status(&self) -> Status {
        if self.failed {
            Status::Failed
        } else if self.finished {
            Status::Finished
        } else {
            self.counters.status
        }
    }

    /// Runs from boundary to boundary, one step of the script's ticks each,
    /// jumping straight to the next limit, frame edge or event. At one
    /// boundary execution errors, power-off and halt take priority, then a
    /// requested frame stop, the deadline and the absolute step budget.
    fn run(&mut self, limit: RunLimit) -> Result<Stop, Error> {
        self.live()?;
        match self.counters.status {
            Status::Halted => return Ok(Stop::Halted),
            Status::Off => return Ok(Stop::PoweredOff),
            _ => {}
        }
        if limit.stop_on_frame && !self.description.features.frame_stops {
            return Err(unsupported("this machine does not stop at frames"));
        }
        for event in std::mem::take(&mut self.pending_input) {
            self.apply_input(event);
        }
        let stop = self.run_loop(limit)?;
        self.battery_checkpoint()?;
        Ok(stop)
    }

    fn frame(&mut self, display: DisplayId) -> Result<Frame<'_>, Error> {
        if display != DisplayId(0) {
            return Err(usage("the test machine has one display, 0"));
        }
        if self.drawn != Some(self.serial) {
            picture::draw(
                &mut self.pixels,
                self.script.width,
                self.script.height,
                self.script.visible,
                &self.counters.shown,
            );
            self.drawn = Some(self.serial);
        }
        Ok(Frame::new(
            self.script.width,
            self.script.height,
            &self.pixels,
            self.script.aspect,
            self.serial,
        )
        .with_visible(self.script.visible))
    }

    fn drain_audio(&mut self, out: &mut Vec<[i16; 2]>) -> Result<(), Error> {
        let Some(sound) = self.script.sound else {
            return Ok(());
        };
        let target = self.audio_target();
        if self.audio_on {
            out.extend((self.audio_next..target).map(|i| sound::sample(i, sound.rate, sound.tone)));
        }
        self.audio_next = target;
        Ok(())
    }

    /// Sound is on from creation. Switching it either way drops the frames
    /// not drained, so the next frame handed over is the one at `now()`.
    fn set_outputs(&mut self, outputs: Outputs) {
        if self.live().is_err() {
            return;
        }
        if outputs.audio != self.audio_on {
            self.audio_on = outputs.audio;
            self.audio_next = self.audio_target();
        }
    }

    fn input(&mut self, event: InputEvent) -> Result<(), Error> {
        self.interactive()?;
        self.validate_input(&event)?;
        self.pending_input.push(event);
        Ok(())
    }

    fn slot_states(&mut self) -> Vec<SlotState> {
        let accepts = self.description.features.media_changes && self.interactive().is_ok();
        self.slots
            .iter_mut()
            .map(|slot| {
                let active = std::mem::take(&mut slot.active);
                SlotState::new(slot.description.id)
                    .with_catalogue(slot.catalogue.clone())
                    .with_inserted(slot.inserted)
                    .with_active(active)
                    .with_accepts_changes(accepts && slot.description.removable)
            })
            .collect()
    }

    /// Keeps the medium's identity only: the test machine reads no medium.
    fn add_medium(&mut self, slot: SlotId, medium: NewMedium<'_>) -> Result<usize, Error> {
        self.interactive()?;
        if !self.description.features.media_additions {
            return Err(unsupported("media additions unavailable"));
        }
        let run = self
            .slots
            .iter_mut()
            .find(|run| run.description.id == slot)
            .ok_or_else(|| usage("no such slot"))?;
        if !run.description.removable {
            return Err(unsupported("this slot's media do not change"));
        }
        if !run.description.formats.iter().any(|f| f == medium.format) {
            return Err(usage("the slot does not take this format"));
        }
        let (writes, kept) = match medium.writes {
            WriteTarget::Kept { record, .. } => {
                if !is_kept_record_name(record) {
                    return Err(usage("a kept record's name starts with kept-"));
                }
                (WriteKind::Kept, Some(record.to_owned()))
            }
            WriteTarget::Discarded => (WriteKind::Discarded, None),
            _ => (WriteKind::ReadOnly, None),
        };
        run.catalogue.push(MediumInfo::new(
            medium.name,
            medium.sha256,
            medium.size,
            medium.format,
            writes,
        ));
        run.kept.push(kept);
        Ok(run.catalogue.len() - 1)
    }

    fn change_medium(&mut self, slot: SlotId, action: MediaAction) -> Result<(), Error> {
        self.interactive()?;
        if !self.description.features.media_changes {
            return Err(unsupported("media selection unavailable"));
        }
        let run = self
            .slots
            .iter_mut()
            .find(|run| run.description.id == slot)
            .ok_or_else(|| usage("no such slot"))?;
        if !run.description.removable {
            return Err(unsupported("this slot's media do not change"));
        }
        let next = match action {
            MediaAction::Eject => None,
            MediaAction::Insert(entry) if entry < run.catalogue.len() => Some(entry),
            MediaAction::Next if !run.catalogue.is_empty() => Some(
                run.inserted
                    .map_or(0, |entry| (entry + 1) % run.catalogue.len()),
            ),
            _ => return Err(usage("invalid media action or catalogue entry")),
        };
        if next == run.inserted {
            return Ok(());
        }
        if let (Some(old), Some(new)) = (run.inserted, next) {
            let previous = &run.catalogue[old];
            let selected = &run.catalogue[new];
            if run.record.is_some()
                && run.kept[old] == run.kept[new]
                && previous.sha256 == selected.sha256
                && previous.size == selected.size
                && previous.format == selected.format
            {
                // Compatible aliases use the exclusive record already held.
                run.inserted = next;
                run.active = true;
                return Ok(());
            }
        }
        // Open before changing the insertion so an ordinary error preserves it.
        let mut record = match next.and_then(|entry| run.kept[entry].as_deref()) {
            Some(name) => match self.storage.open(name) {
                Ok(record) => Some(record),
                Err(error) => {
                    if error.kind == ErrorKind::StorageUncertain {
                        self.failed = true;
                    }
                    return Err(error);
                }
            },
            None => None,
        };
        let mut generation = Vec::new();
        if let Some(record) = &mut record {
            let result = (|| {
                let size = usize::try_from(record.size()?)
                    .map_err(|_| Error::new(ErrorKind::Storage, "record too large"))?;
                generation.resize(size, 0);
                record.read_at(0, &mut generation).map_err(storage_failure)
            })();
            if let Err(error) = result {
                if error.kind == ErrorKind::StorageUncertain {
                    self.failed = true;
                }
                return Err(error);
            }
        }
        run.generation = generation;
        run.record = record;
        run.inserted = next;
        run.active = true;
        Ok(())
    }

    /// A soft reset restarts the frame count; a hard one also counts a
    /// start in the battery memory. Controls and queued input are released.
    fn reset(&mut self, kind: ResetKind) -> Result<(), Error> {
        self.live()?;
        if self.counters.status == Status::Off && kind != ResetKind::Hard {
            return Err(usage("a machine that is switched off has no reset button"));
        }
        if !self.description.features.resets.contains(&kind) {
            return Err(unsupported("this machine does not take that kind of reset"));
        }
        self.pending_input.clear();
        self.counters.held.pads.fill(0);
        self.counters.held.keys.iter_mut().for_each(Vec::clear);
        self.counters
            .held
            .pointers
            .iter_mut()
            .for_each(|pointer| pointer.1 = 0);
        match kind {
            ResetKind::Soft if self.counters.status == Status::Off => {
                Err(usage("a machine that is switched off has no reset button"))
            }
            ResetKind::Hard => {
                if self.battery.is_some() {
                    self.counters.starts = self.counters.starts.saturating_add(1);
                }
                self.restart();
                Ok(())
            }
            _ => {
                self.restart();
                Ok(())
            }
        }
    }

    fn state_identity(&self) -> Option<String> {
        self.description.features.saved_state.then(|| {
            format!(
                "{} {:?}",
                self.identity,
                self.slots
                    .iter()
                    .map(|slot| (&slot.catalogue, &slot.kept, slot.inserted, &slot.generation))
                    .collect::<Vec<_>>()
            )
        })
    }

    fn save_state(&mut self, out: &mut dyn Write) -> Result<(), Error> {
        self.live()?;
        if !self.description.features.saved_state {
            return Err(unsupported("this machine does not save state"));
        }
        out.write_all(&state::encode(
            &self.state_identity().expect("state advertised"),
            &self.counters,
            &self.pending_input,
        ))
        .map_err(|error| {
            Error::new(
                ErrorKind::Storage,
                format!("the state could not be written: {error}"),
            )
        })
    }

    fn load_state(&mut self, input: &mut dyn Read) -> Result<(), Error> {
        self.live()?;
        if !self.description.features.saved_state {
            return Err(unsupported("this machine does not load state"));
        }
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).map_err(|error| {
            Error::new(
                ErrorKind::Storage,
                format!("the state could not be read: {error}"),
            )
        })?;
        let (counters, pending) = state::decode(
            &self.state_identity().expect("state advertised"),
            &self.shape(),
            &bytes,
        )?;
        if counters.now % self.script.step != 0 {
            return Err(Error::new(
                ErrorKind::State,
                "saved time is not a step boundary",
            ));
        }
        for event in &pending {
            self.validate_input(event)
                .map_err(|_| Error::new(ErrorKind::State, "invalid saved input"))?;
        }
        if self.battery.is_none() && counters.starts != 0 {
            return Err(Error::new(
                ErrorKind::State,
                "the saved state has battery memory this machine lacks",
            ));
        }
        if counters.shown != self.counters.shown {
            self.serial += 1;
        }
        self.counters = counters;
        self.pending_input = pending;
        if let Some(battery) = self.battery.as_mut() {
            battery.written_at = battery.written_at.min(self.counters.now);
        }
        self.audio_next = self.audio_target();
        Ok(())
    }

    /// Writes changed battery memory, makes every kept record durable and
    /// releases every record.
    fn finish(&mut self) -> Result<Finished, Error> {
        self.live()?;
        if self
            .battery
            .as_ref()
            .is_some_and(|battery| battery.written_starts != self.counters.starts)
        {
            self.write_battery()?;
        }
        for index in 0..self.slots.len() {
            if let Some(record) = self.slots[index].record.as_mut() {
                if let Err(error) = record.sync() {
                    return Err(self.fail(storage_failure(error)));
                }
            }
        }
        for slot in &mut self.slots {
            slot.record = None;
        }
        self.battery = None;
        self.finished = true;
        Ok(Finished::new())
    }

    fn drain_messages(&mut self, out: &mut Vec<Message>) {
        out.append(&mut self.messages);
    }
}
