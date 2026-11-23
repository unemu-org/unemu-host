// SPDX-License-Identifier: AGPL-3.0-or-later
//! The macOS keyboard tap that hands system shortcuts to the guest.
//!
//! A thin shell around [`crate::system_keys`], which decides what to take and
//! when the tap exists. This module creates an active event tap for key-down
//! and key-up events (Quartz Event Services, `CGEventTapCreate` with
//! `kCGEventTapOptionDefault`, inserted at the head of the filters), reads each
//! event's virtual key code, autorepeat field and Control and Command flags,
//! maps the key code with winit's own table so the guest receives the key the
//! window would have reported, and discards exactly the events the chord
//! filter took. Modifier changes are not in the event mask, so they always
//! reach the window.
//!
//! The tap sits where HID events enter the window server: Mission Control and
//! the application switcher act on their chords before any application
//! receives them, and QEMU's Cocoa display captures them with a tap at this
//! level (QEMU `ui/cocoa.m`, keyboard grab). CGEvent.h says only root may tap
//! there, so the session level is tried when macOS refuses it. Without the
//! Accessibility privilege macOS clears the key bits of the mask and returns
//! no tap at either level.
//!
//! The callback runs on the main run loop, in its common modes. It takes keys
//! only while this application is active and the shell's deliver closure
//! accepts them, and it passes every event unchanged if anything panics.
//! macOS switches a tap off when its callback answers too slowly
//! (`kCGEventTapDisabledByTimeout`), so a stalled emulator delays the keyboard
//! once and then lets it go; the lifetime logic reinstalls the tap, which
//! also checks the privilege again. Removing the tap disables it, removes its
//! run-loop source and invalidates its port before the callback's state is
//! freed; the window server releases the tap of a process that ended with its
//! port, so no tap outlives the application. All unsafe code of the tap stays
//! in this module.

use crate::system_keys::{ChordFilter, Deliver, Disabled, TapBackend, TapKey};
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_core_foundation::{
    kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGPreflightPostEventAccess,
    CGRequestPostEventAccess,
};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::NonNull,
};
use winit::{
    event::ElementState,
    keyboard::{KeyCode, PhysicalKey},
    platform::scancode::PhysicalKeyExtScancode,
};

/// Why macOS stopped calling the tap, when `kind` is one of its notices.
fn disabled_by(kind: CGEventType) -> Option<Disabled> {
    if kind == CGEventType::TapDisabledByTimeout {
        Some(Disabled::Timeout)
    } else if kind == CGEventType::TapDisabledByUserInput {
        Some(Disabled::UserInput)
    } else {
        None
    }
}

/// The key event a tapped key-down or key-up carries: its virtual key code
/// mapped by winit, its autorepeat field and its Control and Command flags.
fn decode(kind: CGEventType, event: &CGEvent) -> Option<TapKey> {
    let state = if kind == CGEventType::KeyDown {
        ElementState::Pressed
    } else if kind == CGEventType::KeyUp {
        ElementState::Released
    } else {
        return None;
    };
    let field = |field| CGEvent::integer_value_field(Some(event), field);
    let code = u32::try_from(field(CGEventField::KeyboardEventKeycode)).ok()?;
    let PhysicalKey::Code(key) = KeyCode::from_scancode(code) else {
        return None;
    };
    let flags = CGEvent::flags(Some(event));
    Some(TapKey {
        key,
        state,
        repeat: field(CGEventField::KeyboardEventAutorepeat) != 0,
        control: flags.contains(CGEventFlags::MaskControl),
        command: flags.contains(CGEventFlags::MaskCommand),
    })
}

/// The state the callback reads. `install` leaks it onto the heap so its
/// address stays fixed while the tap holds it; `remove` frees it.
struct Context {
    filter: RefCell<ChordFilter>,
    deliver: RefCell<Deliver>,
    disabled: Cell<Option<Disabled>>,
}

impl Context {
    /// True when the event must not reach macOS.
    fn handle(&self, kind: CGEventType, event: &CGEvent) -> bool {
        if let Some(reason) = disabled_by(kind) {
            self.disabled.set(Some(reason));
            return false;
        }
        let Some(event) = decode(kind, event) else {
            return false;
        };
        // The tap sees the keys of every application; another one's stay its own.
        let active = MainThreadMarker::new()
            .is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive());
        let (Ok(mut filter), Ok(mut deliver)) =
            (self.filter.try_borrow_mut(), self.deliver.try_borrow_mut())
        else {
            return false;
        };
        filter.take(event, |key| active && deliver(key))
    }
}

/// Quartz calls this for each tapped event and for the tap's disable notices.
///
/// # Safety
///
/// `user_info` is the [`Context`] pointer that `MacTap`'s `install` leaked.
/// Only shared references are made from it, and `remove` invalidates the
/// tap's port before it frees the Context.
unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: see the function's contract; Quartz passes a live event.
    let (context, cg_event) = unsafe { (&*user_info.cast::<Context>(), event.as_ref()) };
    match catch_unwind(AssertUnwindSafe(|| context.handle(kind, cg_event))) {
        Ok(true) => std::ptr::null_mut(),
        _ => event.as_ptr(),
    }
}

struct Installed {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
    /// Owned: leaked by `install`, freed by `remove` once the port is invalid.
    context: NonNull<Context>,
}

/// Frees a Context that `install` leaked.
///
/// # Safety
///
/// `context` came from `install`'s `Box::leak`, is freed only once, and no
/// valid tap port refers to it any more.
unsafe fn free(context: NonNull<Context>) {
    // SAFETY: see the function's contract.
    drop(unsafe { Box::from_raw(context.as_ptr()) });
}

/// The tap, present only between [`TapBackend::install`] and
/// [`TapBackend::remove`].
#[derive(Default)]
pub(crate) struct MacTap {
    installed: Option<Installed>,
}

impl TapBackend for MacTap {
    fn install(&mut self, deliver: Deliver) -> bool {
        self.remove();
        let context = NonNull::from(Box::leak(Box::new(Context {
            filter: RefCell::default(),
            deliver: RefCell::new(deliver),
            disabled: Cell::new(None),
        })));
        let user_info = context.as_ptr().cast::<c_void>();
        let mask: CGEventMask = (1 << CGEventType::KeyDown.0) | (1 << CGEventType::KeyUp.0);
        let Some(port) = [
            CGEventTapLocation::HIDEventTap,
            CGEventTapLocation::SessionEventTap,
        ]
        .into_iter()
        .find_map(|location| {
            // SAFETY: `callback` matches CGEventTapCallBack and only reads
            // `user_info` as the leaked Context, which outlives the port.
            unsafe {
                CGEvent::tap_create(
                    location,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::Default,
                    mask,
                    Some(callback),
                    user_info,
                )
            }
        }) else {
            // SAFETY: no tap was created, so nothing refers to the Context.
            unsafe { free(context) };
            return false;
        };
        let (Some(source), Some(run_loop)) = (
            CFMachPort::new_run_loop_source(None, Some(&port), 0),
            CFRunLoop::main(),
        ) else {
            port.invalidate();
            // SAFETY: the only tap that refers to the Context is invalid now.
            unsafe { free(context) };
            return false;
        };
        // SAFETY: CoreFoundation's constant mode name, read-only.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&port, true);
        self.installed = Some(Installed {
            port,
            source,
            run_loop,
            context,
        });
        true
    }

    fn remove(&mut self) {
        let Some(installed) = self.installed.take() else {
            return;
        };
        CGEvent::tap_enable(&installed.port, false);
        // SAFETY: CoreFoundation's constant mode name, read-only.
        installed
            .run_loop
            .remove_source(Some(&installed.source), unsafe { kCFRunLoopCommonModes });
        installed.source.invalidate();
        installed.port.invalidate();
        // SAFETY: `installed` held the Context `install` leaked, and its port,
        // the only tap that referred to it, is invalid now.
        unsafe { free(installed.context) };
    }

    fn disabled(&self) -> Option<Disabled> {
        self.installed.as_ref().and_then(|installed| {
            // SAFETY: the Context lives until `remove` takes `installed`; the
            // callback makes only shared references to it too.
            unsafe { installed.context.as_ref() }.disabled.get()
        })
    }

    fn has_access(&self) -> bool {
        CGPreflightPostEventAccess()
    }

    fn request_access(&mut self) -> bool {
        // The request shows macOS's Accessibility prompt the first time and
        // returns at once; the user answers later, in System Settings.
        CGPreflightPostEventAccess() || CGRequestPostEventAccess()
    }
}

impl Drop for MacTap {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_graphics::CGEventFlags as Flags;

    /// A synthetic key event, as the tap would receive it.
    fn key_event(code: u16, down: bool, flags: Flags, repeat: bool) -> CFRetained<CGEvent> {
        let event = CGEvent::new_keyboard_event(None, code, down).expect("Quartz creates events");
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::KeyboardEventAutorepeat,
            i64::from(repeat),
        );
        event
    }

    const LEFT_ARROW: u16 = 0x7B;

    #[test]
    fn a_tapped_key_down_or_up_decodes_to_the_window_key_code() {
        let down = key_event(LEFT_ARROW, true, Flags::empty(), false);
        let key = decode(CGEventType::KeyDown, &down).unwrap();
        assert_eq!(
            (key.key, key.state),
            (KeyCode::ArrowLeft, ElementState::Pressed)
        );
        let up = key_event(LEFT_ARROW, false, Flags::empty(), false);
        let key = decode(CGEventType::KeyUp, &up).unwrap();
        assert_eq!(
            (key.key, key.state),
            (KeyCode::ArrowLeft, ElementState::Released)
        );
        // Only key events are decoded.
        assert_eq!(decode(CGEventType::FlagsChanged, &down), None);
    }

    #[test]
    fn control_and_command_flags_decode_separately() {
        for (flags, control, command) in [
            (Flags::empty(), false, false),
            (Flags::MaskControl, true, false),
            (Flags::MaskCommand, false, true),
            (Flags::MaskControl | Flags::MaskCommand, true, true),
            // Option and Shift are neither.
            (Flags::MaskAlternate | Flags::MaskShift, false, false),
        ] {
            let event = key_event(LEFT_ARROW, true, flags, false);
            let key = decode(CGEventType::KeyDown, &event).unwrap();
            assert_eq!((key.control, key.command), (control, command), "{flags:?}");
        }
    }

    #[test]
    fn the_autorepeat_field_decodes_as_a_repeat() {
        let first = key_event(LEFT_ARROW, true, Flags::MaskControl, false);
        assert!(!decode(CGEventType::KeyDown, &first).unwrap().repeat);
        let again = key_event(LEFT_ARROW, true, Flags::MaskControl, true);
        assert!(decode(CGEventType::KeyDown, &again).unwrap().repeat);
    }

    #[test]
    fn disable_notices_are_recorded_and_pass_the_event() {
        let event = key_event(LEFT_ARROW, true, Flags::empty(), false);
        for (kind, reason) in [
            (CGEventType::TapDisabledByTimeout, Disabled::Timeout),
            (CGEventType::TapDisabledByUserInput, Disabled::UserInput),
        ] {
            let context = Context {
                filter: RefCell::default(),
                deliver: RefCell::new(Box::new(|_| panic!("a notice is not a key"))),
                disabled: Cell::new(None),
            };
            assert!(!context.handle(kind, &event));
            assert_eq!(context.disabled.get(), Some(reason));
        }
        assert_eq!(disabled_by(CGEventType::KeyDown), None);
    }
}
