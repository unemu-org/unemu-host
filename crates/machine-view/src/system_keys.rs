// SPDX-License-Identifier: AGPL-3.0-or-later
//! macOS system shortcuts handed to the guest: which key events the opt-in
//! keyboard tap takes from macOS, and when the tap exists.
//!
//! macOS gives several chords without Command to the system: Control with an
//! arrow key (Mission Control, the front application's windows, move a Space),
//! Control with a digit (switch to Desktop N), Control with F1–F8 (keyboard
//! navigation), Control–Space (input sources) and F11 (Show Desktop); see
//! Apple, "Mac keyboard shortcuts", and System Settings › Keyboard › Keyboard
//! Shortcuts. The window server acts on them before the active application
//! receives the key, so a DOS game that fires with Ctrl and moves with the
//! arrows switches Spaces instead. NSApplication presentation options cannot
//! prevent that: they hide the Dock and menu bar or disable the Command–Tab
//! switcher, Force Quit and session termination, never Mission Control (AppKit,
//! NSApplicationPresentationOptions). An active event tap (Quartz Event
//! Services, `CGEventTapCreate`, `kCGEventTapOptionDefault`) sees each key
//! event first and may discard it; it needs the Accessibility privilege.
//!
//! `ChordFilter` decides which events the tap discards: Control with an
//! arrow, navigation key, digit, Space or function key, and every function
//! key, as long as Command is up. Command chords stay with the host because
//! they are the emulator's own shortcuts and application switching. Modifier
//! changes always pass, so the window keeps tracking them. A repeat or release
//! goes where its press went, and a key the shell cannot take passes on to
//! macOS, so neither side keeps a key held. Every taken event enters the
//! window's ordinary key queue, so guest delivery, focus checks and the
//! focus-loss release of held keys stay one code path.
//!
//! [`SystemKeys`] owns the tap's lifetime: off by default, installed only while
//! the user's preference is on and the guest has the keyboard, removed on
//! focus loss, dialogs, pause and drop, reinstalled after macOS switched it off
//! for answering too slowly, and asked for at most once per guest focus when
//! macOS refuses it. The tap itself is a thin macOS shell around these two
//! (`macos_keys`), since no test can create one. See `the machine guide in unemupc`
//! § macOS system shortcuts in games.

use winit::{event::ElementState, keyboard::KeyCode};

/// One key event the shell takes for the guest instead of macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestKey {
    pub key: KeyCode,
    pub state: ElementState,
    pub repeat: bool,
}

/// Hands a taken key to the shell; false when the shell cannot take it now,
/// for example because the guest no longer has the keyboard.
pub type Deliver = Box<dyn FnMut(GuestKey) -> bool>;

/// A key event as the tap reports it, mapped to its physical key, with the
/// modifier flags the system attached to it.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapKey {
    pub key: KeyCode,
    pub state: ElementState,
    pub repeat: bool,
    pub control: bool,
    pub command: bool,
}

/// Keys macOS may take for a system shortcut without any modifier.
#[cfg(any(target_os = "macos", test))]
const FUNCTION_KEYS: [KeyCode; 24] = {
    use KeyCode::*;
    [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24,
    ]
};

/// Keys macOS may take for a system shortcut with Control. Fn with an arrow
/// reports Home, End, Page Up or Page Down, and macOS 15 tiles windows with
/// Fn–Control–arrow.
#[cfg(any(target_os = "macos", test))]
const CONTROL_KEYS: [KeyCode; 19] = {
    use KeyCode::*;
    [
        ArrowLeft, ArrowRight, ArrowUp, ArrowDown, Home, End, PageUp, PageDown, Space, Digit0,
        Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ]
};

/// Whether macOS may take this press for a system shortcut while the guest
/// should receive it.
#[cfg(any(target_os = "macos", test))]
fn collides(key: KeyCode, control: bool, command: bool) -> bool {
    !command && (FUNCTION_KEYS.contains(&key) || (control && CONTROL_KEYS.contains(&key)))
}

/// The keys of one tap's lifetime that went to the guest instead of macOS.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
pub struct ChordFilter {
    taken: std::collections::BTreeSet<KeyCode>,
}

#[cfg(any(target_os = "macos", test))]
impl ChordFilter {
    /// True when the event must not reach macOS: it collided and `deliver`
    /// accepted it. A press that `deliver` refuses passes to macOS together
    /// with its later repeats and release.
    pub fn take(&mut self, event: TapKey, deliver: impl FnOnce(GuestKey) -> bool) -> bool {
        let wanted = match event.state {
            // A key pressed before the chord collided (an arrow held while
            // Ctrl goes down to fire) repeats as the chord macOS acts on.
            ElementState::Pressed if event.repeat => {
                self.taken.contains(&event.key) || collides(event.key, event.control, event.command)
            }
            ElementState::Pressed => collides(event.key, event.control, event.command),
            ElementState::Released => self.taken.contains(&event.key),
        };
        if !wanted {
            if event.state == ElementState::Pressed && !event.repeat {
                self.taken.remove(&event.key);
            }
            return false;
        }
        let delivered = deliver(GuestKey {
            key: event.key,
            state: event.state,
            repeat: event.repeat,
        });
        if delivered && event.state == ElementState::Pressed {
            self.taken.insert(event.key);
        } else {
            self.taken.remove(&event.key);
        }
        delivered
    }
}

/// Why macOS stopped calling an installed tap. Only the macOS tap reports it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disabled {
    /// The callback answered too slowly (`kCGEventTapDisabledByTimeout`).
    Timeout,
    /// The user's input disabled event taps (`kCGEventTapDisabledByUserInput`).
    UserInput,
}

/// The platform half of the tap.
pub trait TapBackend {
    /// Starts taking colliding chords; false when macOS refuses the tap,
    /// normally because the Accessibility privilege is missing.
    fn install(&mut self, deliver: Deliver) -> bool;
    /// Stops taking keys. Every key event reaches macOS again afterwards.
    fn remove(&mut self);
    /// Whether macOS switched the installed tap off, and why.
    fn disabled(&self) -> Option<Disabled>;
    /// Whether macOS reports the Accessibility privilege an active keyboard
    /// tap needs, without asking for it. macOS may not update the answer
    /// while the application runs.
    fn has_access(&self) -> bool;
    /// True when the privilege is granted; otherwise asks macOS to prompt for
    /// it, which answers later.
    fn request_access(&mut self) -> bool;
}

/// The status hint shown when macOS refuses the tap.
pub const REFUSED_HINT: &str = "macOS keeps Ctrl+arrows and other system shortcuts: \
     allow unemu, or the terminal that started it, in System Settings › Privacy & \
     Security › Accessibility";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Off,
    On,
    /// macOS refused or switched off the tap; the next guest focus retries.
    Waiting,
}

/// The tap's lifetime.
pub struct SystemKeys {
    backend: Box<dyn TapBackend>,
    state: State,
    hinted: bool,
    /// Whether macOS created the latest tap asked for; `None` before any.
    created: Option<bool>,
}

impl SystemKeys {
    pub fn new(backend: Box<dyn TapBackend>) -> Self {
        Self {
            backend,
            state: State::Off,
            hinted: false,
            created: None,
        }
    }

    /// The host's tap: Quartz Event Services on macOS, none elsewhere.
    pub fn platform() -> Self {
        #[cfg(target_os = "macos")]
        let backend: Box<dyn TapBackend> = Box::new(crate::macos_keys::MacTap::default());
        #[cfg(not(target_os = "macos"))]
        let backend: Box<dyn TapBackend> = Box::new(Unavailable);
        Self::new(backend)
    }

    /// Brings the tap in line with whether the guest may take system
    /// shortcuts now; run after every input pass. Returns a status hint the
    /// first time in a session that macOS refuses the tap.
    pub fn sync(
        &mut self,
        wanted: bool,
        deliver: impl FnOnce() -> Deliver,
    ) -> Option<&'static str> {
        if !wanted {
            if self.state == State::On {
                self.backend.remove();
            }
            self.state = State::Off;
            return None;
        }
        match (self.state, self.backend.disabled()) {
            (State::On, None) | (State::Waiting, _) => return None,
            (State::On, Some(reason)) => {
                self.backend.remove();
                if reason == Disabled::UserInput {
                    self.state = State::Waiting;
                    return None;
                }
            }
            (State::Off, _) => {}
        }
        let created = self.backend.install(deliver());
        self.created = Some(created);
        if created {
            self.state = State::On;
            return None;
        }
        self.state = State::Waiting;
        (!std::mem::replace(&mut self.hinted, true)).then_some(REFUSED_HINT)
    }

    pub fn request_access(&mut self) -> bool {
        self.backend.request_access()
    }

    /// Whether the tap still lacks the Accessibility privilege. The latest
    /// tap macOS created or refused answers; before any, macOS's own check.
    pub fn access_missing(&self) -> bool {
        self.created
            .map_or_else(|| !self.backend.has_access(), |created| !created)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn installed(&self) -> bool {
        self.state == State::On
    }
}

impl Drop for SystemKeys {
    fn drop(&mut self) {
        if self.state == State::On {
            self.backend.remove();
        }
    }
}

/// Hosts without macOS system shortcuts.
#[cfg(not(target_os = "macos"))]
struct Unavailable;

#[cfg(not(target_os = "macos"))]
impl TapBackend for Unavailable {
    fn install(&mut self, _deliver: Deliver) -> bool {
        false
    }
    fn remove(&mut self) {}
    fn disabled(&self) -> Option<Disabled> {
        None
    }
    fn has_access(&self) -> bool {
        false
    }
    fn request_access(&mut self) -> bool {
        false
    }
}

/// A tap that records what the shell asked of it, for headless tests.
/// A scripted tap for the tests of the shells that hold a [`SystemKeys`].
#[cfg(any(test, feature = "test-support"))]
pub mod fake {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    pub struct Log {
        pub installs: usize,
        pub removes: usize,
        pub access_requests: usize,
        pub refuse: bool,
        pub access_granted: bool,
        pub disabled: Option<Disabled>,
        pub deliver: Option<Deliver>,
    }

    pub struct FakeTap(pub Rc<RefCell<Log>>);

    impl TapBackend for FakeTap {
        fn install(&mut self, deliver: Deliver) -> bool {
            let mut log = self.0.borrow_mut();
            log.installs += 1;
            log.disabled = None;
            if log.refuse {
                return false;
            }
            log.deliver = Some(deliver);
            true
        }
        fn remove(&mut self) {
            let mut log = self.0.borrow_mut();
            log.removes += 1;
            log.deliver = None;
        }
        fn disabled(&self) -> Option<Disabled> {
            self.0.borrow().disabled
        }
        fn has_access(&self) -> bool {
            self.0.borrow().access_granted
        }
        fn request_access(&mut self) -> bool {
            let mut log = self.0.borrow_mut();
            log.access_requests += 1;
            log.access_granted
        }
    }

    pub fn system_keys() -> (SystemKeys, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log::default()));
        (SystemKeys::new(Box::new(FakeTap(log.clone()))), log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ElementState::{Pressed, Released};
    use KeyCode::*;

    fn event(key: KeyCode, state: ElementState, control: bool, command: bool) -> TapKey {
        TapKey {
            key,
            state,
            repeat: false,
            control,
            command,
        }
    }

    fn repeat(key: KeyCode, control: bool) -> TapKey {
        TapKey {
            repeat: true,
            ..event(key, Pressed, control, false)
        }
    }

    /// Runs events through one filter; returns which were taken and what the
    /// guest received.
    fn run(events: &[TapKey]) -> (Vec<bool>, Vec<GuestKey>) {
        let mut filter = ChordFilter::default();
        let mut guest = Vec::new();
        let taken = events
            .iter()
            .map(|&event| {
                filter.take(event, |key| {
                    guest.push(key);
                    true
                })
            })
            .collect();
        (taken, guest)
    }

    #[test]
    fn mission_control_and_desktop_chords_go_to_the_guest() {
        for key in [
            ArrowLeft, ArrowRight, ArrowUp, ArrowDown, Home, End, PageUp, PageDown, Space, Digit1,
            Digit0, F2, F8,
        ] {
            let (taken, guest) = run(&[event(key, Pressed, true, false)]);
            assert_eq!(taken, [true], "Ctrl+{key:?}");
            assert_eq!(
                guest,
                [GuestKey {
                    key,
                    state: Pressed,
                    repeat: false
                }]
            );
        }
        // Show Desktop and other function-key shortcuts need no Control.
        assert_eq!(run(&[event(F11, Pressed, false, false)]).0, [true]);
    }

    #[test]
    fn command_chords_and_plain_keys_stay_on_the_normal_path() {
        for event in [
            // Host shortcuts, Command–Tab and Command–Space stay with macOS.
            event(Tab, Pressed, false, true),
            event(Space, Pressed, false, true),
            event(ArrowLeft, Pressed, true, true),
            event(F11, Pressed, false, true),
            // Plain arrows and Ctrl+letters reach the window as before.
            event(ArrowLeft, Pressed, false, false),
            event(KeyC, Pressed, true, false),
            event(ControlLeft, Pressed, true, false),
        ] {
            let (taken, guest) = run(&[event]);
            assert_eq!(taken, [false], "{event:?}");
            assert!(guest.is_empty());
        }
    }

    #[test]
    fn a_release_follows_its_press_whatever_the_modifiers_are_by_then() {
        // Ctrl goes up before the arrow: the release still goes to the guest.
        let (taken, guest) = run(&[
            event(ArrowLeft, Pressed, true, false),
            repeat(ArrowLeft, false),
            event(ArrowLeft, Released, false, false),
        ]);
        assert_eq!(taken, [true, true, true]);
        assert_eq!(
            guest
                .iter()
                .map(|k| (k.state, k.repeat))
                .collect::<Vec<_>>(),
            [(Pressed, false), (Pressed, true), (Released, false)]
        );
        // Ctrl goes down while a plain arrow is held: that release is the
        // window's, since the window saw the press.
        let (taken, _) = run(&[
            event(ArrowLeft, Pressed, false, false),
            event(ArrowLeft, Released, true, false),
        ]);
        assert_eq!(taken, [false, false]);
    }

    #[test]
    fn a_held_arrow_repeating_under_a_new_ctrl_is_taken_with_its_release() {
        let (taken, guest) = run(&[
            event(ArrowRight, Pressed, false, false),
            repeat(ArrowRight, true),
            event(ArrowRight, Released, true, false),
        ]);
        assert_eq!(taken, [false, true, true]);
        assert_eq!(guest.len(), 2);
    }

    #[test]
    fn a_refused_press_passes_to_macos_with_its_release() {
        let mut filter = ChordFilter::default();
        assert!(!filter.take(event(ArrowUp, Pressed, true, false), |_| false));
        assert!(!filter.take(event(ArrowUp, Released, true, false), |_| {
            panic!("a key macOS saw pressed is released to macOS")
        }));
        // A release the shell refuses passes too, so macOS sees it.
        assert!(filter.take(event(ArrowDown, Pressed, true, false), |_| true));
        assert!(!filter.take(event(ArrowDown, Released, true, false), |_| false));
        assert!(!filter.take(event(ArrowDown, Released, true, false), |_| {
            panic!("no longer held")
        }));
    }

    #[test]
    fn the_tap_is_off_by_default_and_follows_the_guest_focus() {
        let (mut keys, log) = fake::system_keys();
        let deliver = || -> Deliver { Box::new(|_| true) };
        assert!(!keys.installed());
        assert_eq!(keys.sync(false, deliver), None);
        assert_eq!(log.borrow().installs, 0);
        assert_eq!(keys.sync(true, deliver), None);
        assert_eq!(keys.sync(true, deliver), None);
        assert_eq!((log.borrow().installs, log.borrow().removes), (1, 0));
        assert!(keys.installed());
        // Focus loss, a dialog or pause.
        assert_eq!(keys.sync(false, deliver), None);
        assert_eq!((log.borrow().installs, log.borrow().removes), (1, 1));
        assert!(log.borrow().deliver.is_none());
        assert_eq!(keys.sync(true, deliver), None);
        drop(keys);
        assert_eq!((log.borrow().installs, log.borrow().removes), (2, 2));
    }

    #[test]
    fn a_timed_out_tap_is_reinstalled_and_a_user_disabled_one_waits_for_focus() {
        let (mut keys, log) = fake::system_keys();
        let deliver = || -> Deliver { Box::new(|_| true) };
        keys.sync(true, deliver);
        log.borrow_mut().disabled = Some(Disabled::Timeout);
        keys.sync(true, deliver);
        assert_eq!((log.borrow().installs, log.borrow().removes), (2, 1));
        assert!(keys.installed());
        log.borrow_mut().disabled = Some(Disabled::UserInput);
        keys.sync(true, deliver);
        keys.sync(true, deliver);
        assert_eq!((log.borrow().installs, log.borrow().removes), (2, 2));
        assert!(!keys.installed());
        keys.sync(false, deliver);
        keys.sync(true, deliver);
        assert_eq!(log.borrow().installs, 3);
        assert!(keys.installed());
    }

    #[test]
    fn the_latest_tap_creation_says_whether_access_is_missing() {
        let (mut keys, log) = fake::system_keys();
        let deliver = || -> Deliver { Box::new(|_| true) };
        assert!(keys.access_missing(), "macOS's check before any tap");
        log.borrow_mut().access_granted = true;
        assert!(!keys.access_missing());
        // macOS's check can lag; a refused tap is the stronger answer.
        log.borrow_mut().refuse = true;
        keys.sync(true, deliver);
        assert!(keys.access_missing());
        log.borrow_mut().access_granted = false;
        log.borrow_mut().refuse = false;
        keys.sync(false, deliver);
        keys.sync(true, deliver);
        assert!(!keys.access_missing(), "a created tap has the privilege");
    }

    #[test]
    fn a_refused_tap_hints_once_and_retries_only_on_the_next_focus() {
        let (mut keys, log) = fake::system_keys();
        let deliver = || -> Deliver { Box::new(|_| true) };
        log.borrow_mut().refuse = true;
        assert_eq!(keys.sync(true, deliver), Some(REFUSED_HINT));
        assert_eq!(keys.sync(true, deliver), None);
        assert_eq!(log.borrow().installs, 1, "no retry while focused");
        keys.sync(false, deliver);
        assert_eq!(keys.sync(true, deliver), None, "the hint shows once");
        assert_eq!(log.borrow().installs, 2);
        assert_eq!(log.borrow().removes, 0, "nothing was installed");
        log.borrow_mut().refuse = false;
        keys.sync(false, deliver);
        keys.sync(true, deliver);
        assert!(keys.installed());
    }
}
