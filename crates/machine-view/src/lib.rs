// SPDX-License-Identifier: AGPL-3.0-or-later
//! The desktop's session of one running machine, over the front-end contract
//! alone; see `backlog/docs/architecture.md` § Observation and presentation and
//! `backlog/docs/design/core-contract.md` §5 and §8.
//!
//! [`session::Session`] drives a [`unemu_core_api::Machine`]: it paces runs
//! against the host clock ([`pacing`]), plays the drained sound at whatever
//! exact rate the machine states ([`host_audio`]), maps the window's physical
//! keys to keyboard usages ([`keys`]), works the pads of a machine without a
//! keyboard from bound keys ([`pads`]), captures the pointer for a machine
//! with a relative pointing device and aims it for one with an absolute
//! pointing device ([`pointer`](mod@pointer)), resets the machine and saves
//! and loads its state, and takes the drives' activity for their lights.
//! [`screen`] is the geometry a view shows a picture with, and its inverse
//! for the absolute pointer.
//! [`system_keys`] decides which macOS system shortcuts the guest may take,
//! through the opt-in keyboard tap of `macos_keys`.
//!
//! Only the binary owns a window and a host audio stream; the session and
//! its tests stay headless. The host clock is read only here, for pacing and
//! slice deadlines, and never enters the machine. The crate knows no
//! platform, and the boundary check keeps it so (layer `neutral`,
//! `scripts/crate-boundaries.sh`).

pub mod host_audio;
pub mod keys;
#[cfg(target_os = "macos")]
mod macos_keys;
pub mod pacing;
pub mod pads;
pub mod pointer;
pub mod screen;
pub mod session;
pub mod system_keys;

pub use session::{Picture, Session};
