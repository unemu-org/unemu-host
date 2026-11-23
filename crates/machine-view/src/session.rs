// SPDX-License-Identifier: AGPL-3.0-or-later
//! The paced session of one machine in the desktop window (backlog/docs/architecture.md,
//! Observation and presentation; core-contract.md §5).
//!
//! The session reads a monotonic host clock for cadence and slice deadlines;
//! [`crate::pacing`] converts elapsed time to bounded tick targets at the rate
//! the machine describes. The machine advances only inside
//! [`Machine::run`], in chunks of at most a millisecond of guest time, so the
//! host can end a slice between chunks. Input enters only at run boundaries;
//! the picture is the completed boundary's. Audio is drained at each run
//! boundary into a bounded playback ring and never paces execution.
//!
//! Host keys: Command+Shift+M on macOS (Ctrl+Alt+M elsewhere) toggles mute
//! while a host audio device is active. Command+F on macOS (Ctrl+Alt+F
//! elsewhere) inserts the next medium of the first removable slot, wrapping
//! to the first; Command+E (Ctrl+Alt+E) ejects it; Shift selects the second
//! slot. These keys queue media changes for the next run boundary, and their
//! make, repeat and break never reach the guest. A machine with a relative
//! pointer captures it on a client-area click (that click is swallowed);
//! Command+G on macOS (Ctrl+Alt+G elsewhere), focus loss and suspension
//! release capture and the guest's buttons. The window tries Locked then
//! Confined, hides the cursor and shows the release chord in its status bar.
//! Failed grabs leave input free; application exit restores the pointer.
//! Focus loss releases all held keys; stopped-machine input is ignored. See
//! `backlog/docs/architecture.md` § Core boundary and host timing.
//!
//! A machine with a keyboard port gets the guest's keys there. A machine
//! without one but with pad ports gets its pads from the keys bound in
//! [`Session::set_pad_bindings`] (core-contract.md §13.3, "Pads from the
//! keyboard"): a bound key becomes a pad event for its player's port, if
//! that port lists the control, and nothing else; host repeats are dropped,
//! and a control stays held while any of its keys is. Host chords are taken
//! first, as for a keyboard.
//!
//! A machine with an absolute pointer on the shown display is aimed at, not
//! captured ([`Session::pointer_at`], core-contract.md §13.3, "The absolute
//! pointer"): the view says where the host pointer is on the picture, with
//! the geometry of [`crate::screen`], and the session sends the place when
//! it changes and `None` when the pointer leaves the screen or the window
//! loses focus. The machine's resets ([`Session::reset`]) and saved states
//! ([`Session::save_state`], [`Session::load_state`]) go through the session,
//! so it keeps its pacing, sound and held input in step with the machine.

use crate::{
    host_audio::HostAudio,
    keys::key_usage,
    pacing::{Pacing, Speed},
    pads::PadBindings,
    pointer::{Aiming, PointerCapture},
};
use std::{
    error::Error,
    io::{self, Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};
use unemu_core_api::{
    DisplayId, ErrorKind, FrameArea, InputEvent, InputKind, Key, KeyState, Machine, MediaAction,
    Message, PadControl, PortId, ResetKind, RunLimit, SlotId, Status, Stop, Ticks,
};
use winit::{
    event::{ElementState, MouseButton},
    keyboard::KeyCode,
    window::{CursorGrabMode, Window},
};

/// One picture of the machine's display, copied out of the machine at the
/// end of an advance for the window's texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// `width * height` pixels, rows top to bottom, each `0x00RRGGBB`.
    pub pixels: Vec<u32>,
    /// The shape of the whole picture on the emulated display.
    pub aspect: (u32, u32),
    /// Changes whenever the machine's picture changed.
    pub serial: u64,
    /// The part of the picture the emulated display showed for certain, as
    /// the machine states it; the whole picture when it states nothing. The
    /// PC's screen shows the whole picture; other machines' views show this
    /// part unless overscan is shown ([`crate::screen::layout`]).
    pub visible: FrameArea,
}

/// One machine in the window, paced against the host clock.
///
/// The session is written against [`Machine`] alone. A shell that needs more
/// of its machine than the contract (platform dialogs, say) names the
/// machine's type and reaches it through [`Session::machine_mut`]; one that
/// needs nothing more holds a `Session<dyn Machine>`.
pub struct Session<M: Machine + ?Sized = dyn Machine> {
    machine: Box<M>,
    display: DisplayId,
    keyboard: Option<PortId>,
    /// The pad ports, one per player in ascending port number, each with
    /// the controls it lists.
    pads: Vec<(PortId, Vec<PadControl>)>,
    /// The bound keys held now, for a machine whose pads take the keys.
    pad_keys: Vec<Key>,
    /// Which host key works which pad control: [`PadBindings::defaults`]
    /// until the application sets its own with [`Session::set_pad_bindings`].
    pad_bindings: PadBindings,
    picture: Option<Picture>,
    pub audio: HostAudio,
    audio_frames: Vec<[i16; 2]>,
    mute_key_held: bool,
    mute_modifiers: [bool; 4],
    suppress_m: bool,
    media_keys: [bool; 2],
    media_held: [bool; 2],
    shift_keys: [bool; 2],
    /// Media changes waiting for the next run boundary.
    pub media_events: Vec<(SlotId, MediaAction)>,
    /// The removable slots the media keys change: the first, then the second.
    media_slots: Vec<SlotId>,
    audio_error_reported: bool,
    pub window: Option<Arc<Window>>,
    pub running: bool,
    /// The guest halted with interrupts off, as a clone AT BIOS does after
    /// "No ROM BASIC" while it waits for the reset button. Its media stay
    /// open: a floppy can still go in, and [`Session::reset_halted`] runs
    /// POST again on the same session.
    pub halted: bool,
    /// The shell reported a host error for this session: it takes no more
    /// input, but Stop still finishes the media of a halted one.
    pub host_failed: bool,
    pub mouse: PointerCapture,
    /// The absolute pointer, for a machine that has one on the display.
    aim: Aiming,
    pub error: Option<Box<dyn Error>>,
    pub pacing: Pacing,
    pub started: Option<Instant>,
    pub next_paint: Option<Duration>,
    pub occluded_since: Option<Duration>,
    /// Run every slice to its paced virtual target, as an infinitely fast host
    /// would, so a test that drives the shell sees the same machine on a loaded
    /// host. At Unlimited speed a slice then ends only when the guest stops.
    #[cfg(any(test, feature = "test-support"))]
    pub full_slices: bool,
    /// How often an advance copied a new picture out of the machine.
    #[cfg(any(test, feature = "test-support"))]
    pub picture_copies: u64,
}

impl<M: Machine + ?Sized> Session<M> {
    /// Start a session on `display`, writing the machine's first messages to
    /// `warnings`.
    pub fn new(
        mut machine: Box<M>,
        display: DisplayId,
        warnings: &mut impl io::Write,
    ) -> Result<Self, Box<dyn Error>> {
        let mut messages = Vec::new();
        machine.drain_messages(&mut messages);
        write_messages(&messages, warnings)?;
        let description = machine.description();
        let keyboard = description
            .inputs
            .iter()
            .find(|port| matches!(port.kind, InputKind::Keyboard))
            .map(|port| port.id);
        let pointer = description
            .inputs
            .iter()
            .find(|port| matches!(port.kind, InputKind::RelativePointer { .. }))
            .map(|port| port.id);
        let aimed = description.inputs.iter().find_map(|port| match port.kind {
            InputKind::AbsolutePointer {
                display: aimed,
                buttons,
            } if aimed == display => Some((port.id, buttons)),
            _ => None,
        });
        let mut pads: Vec<_> = description
            .inputs
            .iter()
            .filter_map(|port| match &port.kind {
                InputKind::Pad { controls } => Some((port.id, controls.clone())),
                _ => None,
            })
            .collect();
        pads.sort_by_key(|(port, _)| *port);
        let media_slots = description
            .slots
            .iter()
            .filter(|slot| slot.removable)
            .map(|slot| slot.id)
            .take(2)
            .collect();
        let pacing = Pacing::new(description.clock);
        let audio = description
            .audio
            .map_or_else(HostAudio::new, |sound| HostAudio::for_source(sound.rate));
        Ok(Self {
            machine,
            display,
            keyboard,
            pads,
            pad_keys: Vec::new(),
            pad_bindings: PadBindings::defaults(),
            picture: None,
            audio,
            audio_frames: Vec::new(),
            mute_key_held: false,
            mute_modifiers: [false; 4],
            suppress_m: false,
            media_keys: [false; 2],
            media_held: [false; 2],
            shift_keys: [false; 2],
            media_events: Vec::new(),
            media_slots,
            audio_error_reported: false,
            window: None,
            running: true,
            halted: false,
            host_failed: false,
            mouse: PointerCapture::new(pointer),
            aim: Aiming::new(aimed),
            error: None,
            pacing,
            started: None,
            next_paint: None,
            occluded_since: None,
            #[cfg(any(test, feature = "test-support"))]
            full_slices: false,
            #[cfg(any(test, feature = "test-support"))]
            picture_copies: 0,
        })
    }

    /// Whether no part of a host mute chord is held.
    #[cfg(any(test, feature = "test-support"))]
    pub fn mute_chord_idle(&self) -> bool {
        !self.mute_modifiers.contains(&true) && !self.mute_key_held
    }

    /// The machine, through the contract or as its own type.
    pub fn machine(&self) -> &M {
        &self.machine
    }

    /// The machine, mutably; see [`Session`].
    pub fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }

    /// The display the session shows.
    pub fn display(&self) -> DisplayId {
        self.display
    }

    /// The picture of the latest advance.
    pub fn picture(&self) -> Option<&Picture> {
        self.picture.as_ref()
    }

    pub fn advance(&mut self, elapsed: Duration) -> Result<(), Box<dyn Error>> {
        self.advance_with_warnings(elapsed, &mut io::stderr())
    }

    pub fn advance_with_warnings(
        &mut self,
        elapsed: Duration,
        warnings: &mut impl io::Write,
    ) -> Result<(), Box<dyn Error>> {
        #[cfg(any(test, feature = "test-support"))]
        if self.full_slices {
            return self.advance_fully_with_warnings(elapsed, warnings);
        }
        let started = Instant::now();
        self.advance_with_slice_clock(elapsed, warnings, || started.elapsed())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn advance_fully(&mut self, elapsed: Duration) -> Result<(), Box<dyn Error>> {
        self.advance_fully_with_warnings(elapsed, &mut io::sink())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn advance_fully_with_warnings(
        &mut self,
        elapsed: Duration,
        warnings: &mut impl io::Write,
    ) -> Result<(), Box<dyn Error>> {
        self.advance_with_slice_clock(elapsed, warnings, || Duration::ZERO)
    }

    /// Advance to the paced target of host time `elapsed`, one chunk at a
    /// time while the host slice lasts, then take the completed boundary's
    /// picture. A guest error stops the session and keeps its picture; a host
    /// failure (storage, presentation) is returned.
    pub fn advance_with_slice_clock(
        &mut self,
        elapsed: Duration,
        warnings: &mut impl io::Write,
        mut spent: impl FnMut() -> Duration,
    ) -> Result<(), Box<dyn Error>> {
        self.audio.check_failure(warnings);
        if !self.running || self.occluded_since.is_some() {
            return Ok(());
        }
        // Refresh consumer availability before devices can prune new history.
        self.capture_audio(warnings)?;
        let frame_period = self.pacing.frame_period(elapsed);
        let target = self.pacing.target(elapsed, self.machine.now().0);
        while self.running
            && target.is_none_or(|target| self.machine.now().0 < target)
            && self.pacing.slice_open(spent(), frame_period)
        {
            let chunk = self.pacing.chunk_target(self.machine.now().0, target);
            self.apply_media_events()?;
            let stop = self.machine.run(RunLimit::until(Ticks(chunk)));
            let mut messages = Vec::new();
            self.machine.drain_messages(&mut messages);
            match stop {
                Ok(Stop::Halted) => {
                    self.running = false;
                    self.halted = true;
                    self.capture_audio(warnings)?;
                    eprintln!("machine halted");
                }
                Ok(Stop::PoweredOff) => {
                    self.running = false;
                    self.capture_audio(warnings)?;
                }
                Ok(_) => self.capture_audio(warnings)?,
                Err(error) if error.kind == ErrorKind::Guest => {
                    self.running = false;
                    eprintln!("machine stopped: {error}");
                    self.error = Some(error.into());
                }
                Err(error) => {
                    if self.machine.status() == Status::Halted {
                        // The guest halted, but making its writes durable
                        // failed: the session stays open for Stop.
                        self.running = false;
                        self.halted = true;
                    }
                    let _ = write_messages(&messages, warnings);
                    return Err(error.into());
                }
            }
            let _ = write_messages(&messages, warnings);
        }
        if !self.running {
            self.release_pointer();
            // A halted guest can still be reset, so its session ends at Stop.
            if self.error.is_none() && !self.halted {
                self.finish(warnings)?;
            }
        }
        self.pacing.observe(elapsed, self.machine.now().0);
        // Even a guest error must leave the screen at its last completed boundary.
        self.copy_picture()?;
        Ok(())
    }

    /// Copy the machine's picture out when it changed.
    fn copy_picture(&mut self) -> Result<(), unemu_core_api::Error> {
        let frame = self.machine.frame(self.display)?;
        if self
            .picture
            .as_ref()
            .is_none_or(|picture| picture.serial != frame.serial)
        {
            self.picture = Some(Picture {
                width: frame.width,
                height: frame.height,
                pixels: frame.pixels.to_vec(),
                aspect: frame.aspect,
                serial: frame.serial,
                visible: frame.visible,
            });
            #[cfg(any(test, feature = "test-support"))]
            {
                self.picture_copies += 1;
            }
        }
        Ok(())
    }

    /// The slots that were busy since the last call, for the drive lights.
    pub fn take_slot_activity(&mut self) -> Vec<SlotId> {
        self.machine
            .slot_states()
            .into_iter()
            .filter(|slot| slot.active)
            .map(|slot| slot.id)
            .collect()
    }

    /// Apply the queued media changes. A slot that cannot change now is
    /// skipped; any other failure is returned.
    pub fn apply_media_events(&mut self) -> Result<(), unemu_core_api::Error> {
        let events = std::mem::take(&mut self.media_events);
        for (slot, action) in events {
            match self.machine.change_medium(slot, action) {
                Err(error) if error.kind == ErrorKind::Usage => {}
                result => result?,
            }
        }
        Ok(())
    }

    /// Drain the sound up to now into the host ring. An unavailable or
    /// failing audio observation is reported once and playback continues; a
    /// failure of the machine's storage is returned.
    fn capture_audio(&mut self, warnings: &mut impl io::Write) -> Result<(), Box<dyn Error>> {
        self.machine
            .set_outputs(unemu_core_api::Outputs::NONE.with_audio(self.audio.enabled()));
        self.audio_frames.clear();
        let result = self.machine.drain_audio(&mut self.audio_frames);
        self.audio.push(&self.audio_frames);
        match result {
            // A host write of unknown outcome fails the session as a failed
            // one does: the machine must be created again before it writes.
            Err(error)
                if matches!(error.kind, ErrorKind::Storage | ErrorKind::StorageUncertain) =>
            {
                return Err(error.into())
            }
            Err(error) => {
                if !self.audio_error_reported {
                    let _ = writeln!(warnings, "audio observation failed: {error}; continuing");
                    self.audio_error_reported = true;
                }
            }
            Ok(()) => {}
        }
        Ok(())
    }

    /// End the machine's session cleanly, writing its warnings once.
    pub fn finish(&mut self, warnings: &mut impl io::Write) -> Result<(), Box<dyn Error>> {
        let result = self.machine.finish();
        let mut messages = Vec::new();
        self.machine.drain_messages(&mut messages);
        let _ = write_messages(&messages, warnings);
        let finished = result?;
        for warning in &finished.warnings {
            let _ = writeln!(warnings, "warning: {warning}");
        }
        Ok(())
    }

    /// Whether the drives take media changes now: while the guest runs, and
    /// while it waits halted for a reset.
    pub fn takes_media(&self) -> bool {
        (self.running || self.halted) && self.error.is_none() && !self.host_failed
    }

    /// Whether Ctrl+Alt+Del restarts the machine rather than reaching the
    /// guest: it halted with interrupts off and no error ended the session.
    pub fn awaits_reset(&self) -> bool {
        self.halted && self.error.is_none() && !self.host_failed
    }

    /// Restart a halted machine with its reset ([`ResetKind::Soft`]). Queued
    /// media changes go in first. The reset takes no virtual time, so the run
    /// stays deterministic.
    pub fn reset_halted(&mut self, diagnostics: &mut impl io::Write) -> Result<(), Box<dyn Error>> {
        if !self.awaits_reset() {
            return Ok(());
        }
        self.apply_media_events()?;
        let reset = self.machine.reset(ResetKind::Soft);
        let mut messages = Vec::new();
        self.machine.drain_messages(&mut messages);
        let _ = write_messages(&messages, diagnostics);
        reset?;
        // The reset released the pads; a key still down must press again.
        self.pad_keys.clear();
        self.halted = false;
        self.running = true;
        Ok(())
    }

    /// The kinds of reset the machine lists, for the Reset and Power cycle
    /// controls.
    pub fn resets(&self) -> &'static [ResetKind] {
        self.machine.description().features.resets
    }

    /// Reset the machine by `kind` at host time `elapsed`, while it runs or
    /// waits halted. Queued media changes go in first, and the machine's
    /// messages go to `diagnostics`. A reset takes no virtual time, so a
    /// running machine's pacing goes on as it was; held keys and buttons stay
    /// held, as the machine keeps them, except that a hard reset starts the
    /// absolute pointer afresh at the host pointer's place. A halted machine
    /// runs again as its status says, paced from `elapsed`, so the time it
    /// waited is not caught up. The machine's refusal (a kind it does not
    /// list is `Unsupported`, one not possible now is `Usage`) is returned
    /// for the caller to show, and leaves the session as it was.
    pub fn reset(
        &mut self,
        kind: ResetKind,
        elapsed: Duration,
        diagnostics: &mut impl io::Write,
    ) -> Result<(), unemu_core_api::Error> {
        self.check_live()?;
        self.apply_media_events()?;
        let reset = self.machine.reset(kind);
        let mut messages = Vec::new();
        self.machine.drain_messages(&mut messages);
        let _ = write_messages(&messages, diagnostics);
        reset?;
        if kind == ResetKind::Hard {
            self.aim.restate();
            self.forward_pointer();
        }
        if self.halted {
            // Keys that came up while it was halted reached nobody; the
            // next press of any key must be fresh.
            self.pad_keys.clear();
            let status = self.machine.status();
            self.halted = status == Status::Halted;
            self.running = status == Status::Ready;
            self.pacing.resync(elapsed, self.machine.now().0);
        }
        Ok(())
    }

    /// Whether the machine saves and loads state, for the Save state and
    /// Load state controls.
    pub fn saves_state(&self) -> bool {
        self.machine.description().features.saved_state
    }

    /// The token a saved state of this machine carries; a state loads only
    /// into a machine with an equal one. `None` when it saves no state.
    pub fn state_identity(&self) -> Option<String> {
        self.machine.state_identity()
    }

    /// Write the machine's whole state at the current boundary, while it
    /// runs or waits halted.
    pub fn save_state(&mut self, out: &mut dyn Write) -> Result<(), unemu_core_api::Error> {
        self.check_live()?;
        self.machine.save_state(out)
    }

    /// Replace the machine's state with a saved one, at host time `elapsed`,
    /// while it runs or waits halted.
    ///
    /// A refusal is returned for the caller to show, and leaves the machine
    /// and the session as they were: [`ErrorKind::State`] for a state of
    /// another identity or a damaged one. Loaded, the machine's virtual time
    /// jumps, so pacing is anchored again at `elapsed`; the sound the session
    /// queued before the load is dropped, as the machine drops what it had
    /// not handed over; the screen takes the loaded picture at once; and the
    /// machine is told to release everything, so no control stays held under
    /// a key the host does not hold and the next press is fresh, and the
    /// absolute pointer is told the host pointer's place again. A state saved
    /// while the machine waited halted leaves it halted again, with the
    /// pointer released as when a machine stops; any status but `Ready` and
    /// `Halted` leaves the session stopped.
    pub fn load_state(
        &mut self,
        state: &mut dyn Read,
        elapsed: Duration,
    ) -> Result<(), unemu_core_api::Error> {
        self.check_live()?;
        self.machine.load_state(state)?;
        self.audio.discard_pending();
        self.pacing.resync(elapsed, self.machine.now().0);
        self.pad_keys.clear();
        self.machine.input(InputEvent::ReleaseAll);
        self.mouse.buttons_released();
        // ReleaseAll leaves an absolute pointer where the saved state had it.
        self.aim.restate();
        self.forward_pointer();
        let status = self.machine.status();
        self.halted = status == Status::Halted;
        self.running = status == Status::Ready;
        if !self.running {
            self.release_pointer();
        }
        self.copy_picture()
    }

    /// `Usage` unless the machine runs or waits halted and nothing ended
    /// its session.
    fn check_live(&self) -> Result<(), unemu_core_api::Error> {
        if (self.running || self.halted) && self.error.is_none() && !self.host_failed {
            Ok(())
        } else {
            Err(unemu_core_api::Error::new(
                ErrorKind::Usage,
                "the machine is not running",
            ))
        }
    }

    /// Which host key works which pad control.
    pub fn pad_bindings(&self) -> &PadBindings {
        &self.pad_bindings
    }

    /// Take a new table of pad keys. Every control a held key works is
    /// released first, so no control stays down under a key that no longer
    /// works it, and the next press of any key is fresh.
    pub fn set_pad_bindings(&mut self, bindings: PadBindings) {
        let mut released: Vec<(usize, PadControl)> = Vec::new();
        for key in std::mem::take(&mut self.pad_keys) {
            if let Some(held) = self.pad_bindings.control(key) {
                if !released.contains(&held) {
                    released.push(held);
                }
            }
        }
        if self.running && self.error.is_none() {
            for (player, control) in released {
                if let Some(&(port, _)) = self.pads.get(player) {
                    self.machine.input(InputEvent::Pad {
                        port,
                        control,
                        pressed: false,
                    });
                }
            }
        }
        self.pad_bindings = bindings;
    }

    pub fn occlusion_changed(&mut self, occluded: bool, elapsed: Duration) {
        if occluded {
            self.occluded_since.get_or_insert(elapsed);
            self.next_paint = None;
        } else if self.occluded_since.take().is_some() {
            self.pacing.resync(elapsed, self.machine.now().0);
            self.next_paint = Some(elapsed);
        }
    }

    pub fn select_speed(&mut self, speed: Speed, elapsed: Duration) {
        self.pacing.select(speed, elapsed, self.machine.now().0);
        self.audio
            .set_speed_muted(!self.pacing.effective().audio_enabled());
    }

    pub fn hold_fast_forward(&mut self, held: bool, elapsed: Duration) {
        self.pacing.hold(held, elapsed, self.machine.now().0);
        self.audio
            .set_speed_muted(!self.pacing.effective().audio_enabled());
    }

    /// Re-anchor pacing at host time `elapsed`, as after a pause.
    pub fn resync(&mut self, elapsed: Duration) {
        self.pacing.resync(elapsed, self.machine.now().0);
    }

    pub fn speed_status(&self) -> String {
        if self.running {
            self.pacing.status()
        } else {
            format!("Speed {}", self.pacing.selected().label())
        }
    }

    /// A key for the guest that bypasses the session's host chords (the
    /// system-shortcut path on macOS), while the machine runs.
    pub fn guest_key(&mut self, key: KeyCode, state: ElementState) {
        if self.running && self.error.is_none() {
            self.send_key(key, state);
        }
    }

    fn send_key(&mut self, key: KeyCode, state: ElementState) {
        let Some(key) = key_usage(key) else {
            return;
        };
        let Some(port) = self.keyboard else {
            self.send_pad(key, state == ElementState::Pressed);
            return;
        };
        self.machine.input(InputEvent::Key {
            port,
            key,
            state: if state == ElementState::Pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
        });
    }

    /// Work the pad control `key` is bound to, if its player's port lists
    /// it. A press of a held key is a host repeat; a control changes only
    /// with the first of its keys pressed and the last released.
    fn send_pad(&mut self, key: Key, pressed: bool) {
        let Some((player, control)) = self.pad_bindings.control(key) else {
            return;
        };
        let Some((port, controls)) = self.pads.get(player) else {
            return;
        };
        if !controls.contains(&control) {
            return;
        }
        let port = *port;
        let held = self.pad_keys.contains(&key);
        if pressed == held {
            return;
        }
        if pressed {
            self.pad_keys.push(key);
        } else {
            self.pad_keys.retain(|&other| other != key);
        }
        let bindings = &self.pad_bindings;
        let still_held = self
            .pad_keys
            .iter()
            .any(|&other| other != key && bindings.control(other) == Some((player, control)));
        if !still_held {
            self.machine.input(InputEvent::Pad {
                port,
                control,
                pressed,
            });
        }
    }

    /// Hand the pointers' queued guest events to the machine.
    fn forward_pointer(&mut self) {
        for event in self.mouse.take_events() {
            self.machine.input(event);
        }
        for event in self.aim.take_events() {
            self.machine.input(event);
        }
    }

    /// Whether the machine has an absolute pointer on the shown display: the
    /// view then aims instead of capturing, and shows a cross-hair over the
    /// screen.
    pub fn aims(&self) -> bool {
        self.aim.aims()
    }

    /// Where the host pointer is on the whole picture, as
    /// [`crate::screen::ScreenLayout::pointer_position`] gives it: `None`
    /// once it leaves the screen's rectangle. Only a change reaches the
    /// machine.
    pub fn pointer_at(&mut self, position: Option<(u16, u16)>) {
        if self.running && self.error.is_none() {
            self.aim.moved(position);
            self.forward_pointer();
        }
    }

    pub fn key_event(&mut self, key: KeyCode, state: ElementState) {
        let modifier = match key {
            KeyCode::ControlLeft => Some(0),
            KeyCode::ControlRight => Some(1),
            KeyCode::AltLeft => Some(2),
            KeyCode::AltRight => Some(3),
            _ => None,
        };
        if let Some(index) = modifier {
            self.mute_modifiers[index] = state == ElementState::Pressed;
        }
        match key {
            KeyCode::ShiftLeft => self.shift_keys[0] = state == ElementState::Pressed,
            KeyCode::ShiftRight => self.shift_keys[1] = state == ElementState::Pressed,
            _ => {}
        }
        if let Some(index) = match key {
            KeyCode::KeyF => Some(0),
            KeyCode::KeyE => Some(1),
            _ => None,
        } {
            let fresh = state == ElementState::Pressed && !self.media_held[index];
            self.media_held[index] = state == ElementState::Pressed;
            if self.media_keys[index] {
                if state == ElementState::Released {
                    self.media_keys[index] = false;
                }
                return;
            }
            if fresh
                && self.mute_modifiers[..2].contains(&true)
                && self.mute_modifiers[2..].contains(&true)
            {
                self.media_keys[index] = true;
                if self.takes_media() {
                    let second = usize::from(self.shift_keys.contains(&true));
                    if let Some(&slot) = self.media_slots.get(second) {
                        self.media_events.push((
                            slot,
                            if index == 0 {
                                MediaAction::Next
                            } else {
                                MediaAction::Eject
                            },
                        ));
                    }
                }
                return;
            }
        }
        if key == KeyCode::KeyM {
            let fresh = state == ElementState::Pressed && !self.mute_key_held;
            self.mute_key_held = state == ElementState::Pressed;
            if self.suppress_m {
                self.suppress_m = state != ElementState::Released;
                return;
            }
            if fresh
                && self.audio.available()
                && self.mute_modifiers[..2].contains(&true)
                && self.mute_modifiers[2..].contains(&true)
            {
                self.audio.toggle_mute();
                self.suppress_m = true;
                return;
            }
        }
        if self.running && self.error.is_none() {
            let was_captured = self.mouse.captured();
            if !self.mouse.key_event(key, state) {
                self.send_key(key, state);
            }
            self.forward_pointer();
            if was_captured && !self.mouse.captured() {
                self.restore_pointer();
            }
        }
    }

    pub fn focus_changed(&mut self, focused: bool) {
        if !focused {
            self.media_keys = [false; 2];
            self.media_held = [false; 2];
            self.shift_keys = [false; 2];
            self.mute_key_held = false;
            self.mute_modifiers = [false; 4];
            self.suppress_m = false;
            self.pad_keys.clear();
        }
        let release_all = !focused && self.running && self.error.is_none();
        let was_captured = self.mouse.captured();
        self.mouse.focus_changed(focused, release_all);
        self.forward_pointer();
        if was_captured && !self.mouse.captured() {
            self.restore_pointer();
        }
        if release_all {
            self.machine.input(InputEvent::ReleaseAll);
            // ReleaseAll leaves an absolute pointer's place.
            self.aim.focus_lost();
            self.forward_pointer();
        } else if !focused {
            self.aim.forget();
        }
    }

    pub fn mouse_button(&mut self, button: MouseButton, state: ElementState) {
        let grab = Self::host_grab(self.window.clone());
        self.mouse_button_with_grab(button, state, grab);
    }

    fn host_grab(
        window: Option<Arc<Window>>,
    ) -> impl FnOnce() -> Result<(), winit::error::ExternalError> {
        move || {
            if let Some(window) = window {
                window
                    .set_cursor_grab(CursorGrabMode::Locked)
                    .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))?;
            }
            Ok(())
        }
    }

    pub fn mouse_button_with_grab(
        &mut self,
        button: MouseButton,
        state: ElementState,
        grab: impl FnOnce() -> Result<(), winit::error::ExternalError>,
    ) {
        if self.running && self.error.is_none() {
            if self.mouse.button_event(button, state) {
                self.complete_grab(grab);
            }
            self.aim.button_event(button, state);
        }
        self.forward_pointer();
    }

    /// Resume a capture that a shell question interrupted, without a click.
    pub fn recapture_pointer(&mut self) {
        if self.running && self.error.is_none() && self.mouse.recapture() {
            let grab = Self::host_grab(self.window.clone());
            self.complete_grab(grab);
        }
    }

    fn complete_grab(&mut self, grab: impl FnOnce() -> Result<(), winit::error::ExternalError>) {
        if let Err(error) = grab() {
            eprintln!("cannot capture pointer: {error}");
            self.release_pointer();
            return;
        }
        if let Some(window) = &self.window {
            window.set_cursor_visible(false);
        }
    }

    pub fn mouse_motion(&mut self, dx: f64, dy: f64) {
        if self.running && self.error.is_none() {
            self.mouse.motion(dx, dy);
            self.forward_pointer();
        }
    }

    fn restore_pointer(&mut self) {
        if let Some(window) = &self.window {
            if let Err(error) = window.set_cursor_grab(CursorGrabMode::None) {
                eprintln!("cannot release pointer: {error}");
            }
            window.set_cursor_visible(true);
        }
    }

    pub fn release_pointer(&mut self) {
        let was_captured = self.mouse.captured();
        self.mouse.release();
        self.forward_pointer();
        if was_captured {
            self.restore_pointer();
        }
    }
}

/// Every message on its own line, as the core wrote it.
fn write_messages(messages: &[Message], out: &mut impl io::Write) -> io::Result<()> {
    for message in messages {
        writeln!(out, "{}", message.text)?;
    }
    Ok(())
}

#[cfg(test)]
mod core_tests;
#[cfg(test)]
mod tests;
