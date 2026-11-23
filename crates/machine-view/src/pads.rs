// SPDX-License-Identifier: AGPL-3.0-or-later
//! Game pads from the host keyboard: which key works which control of which
//! player (core-contract.md §13.2, "Pads", and §13.3, "Pads from the
//! keyboard").
//!
//! The table is front-end data: no core sees it, and the same table serves
//! every core. Players are the machine's ports of kind `Pad` in ascending
//! port number, counted from 0 here: player 0 is the first pad port. Keys
//! are the contract's [`Key`] usages; an application stores a binding by the
//! key's [`crate::keys::name`], so its file depends on no toolkit.

use unemu_core_api::{Key, PadControl};

/// Which host keys work which pad control, per player. A key works at most
/// one control; a control may have several keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadBindings {
    /// Every binding as (player, control, key), in the order of
    /// [`PadBindings::entries`].
    bindings: Vec<(usize, PadControl, Key)>,
}

/// The controls in the order a list of bindings shows them.
const CONTROLS: [PadControl; 12] = [
    PadControl::Up,
    PadControl::Down,
    PadControl::Left,
    PadControl::Right,
    PadControl::A,
    PadControl::B,
    PadControl::X,
    PadControl::Y,
    PadControl::L,
    PadControl::R,
    PadControl::Start,
    PadControl::Select,
];

fn order(control: PadControl) -> usize {
    CONTROLS
        .iter()
        .position(|&known| known == control)
        .unwrap_or(CONTROLS.len())
}

impl Default for PadBindings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl PadBindings {
    /// No key works any control.
    pub fn empty() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Player 0 (the first pad): the arrow keys are the directions; `X` is
    /// A and `Z` is B; `S` is X and `A` is Y; `Q` and `W` are L and R; Enter
    /// is Start and the right Shift is Select. No other player has keys.
    pub fn defaults() -> Self {
        let mut bindings = Self::empty();
        for (control, key) in [
            (PadControl::Up, Key::ARROW_UP),
            (PadControl::Down, Key::ARROW_DOWN),
            (PadControl::Left, Key::ARROW_LEFT),
            (PadControl::Right, Key::ARROW_RIGHT),
            (PadControl::A, Key::X),
            (PadControl::B, Key::Z),
            (PadControl::X, Key::S),
            (PadControl::Y, Key::A),
            (PadControl::L, Key::Q),
            (PadControl::R, Key::W),
            (PadControl::Start, Key::ENTER),
            (PadControl::Select, Key::SHIFT_RIGHT),
        ] {
            bindings.bind(0, control, key);
        }
        bindings
    }

    /// The player and control `key` works, if any.
    pub fn control(&self, key: Key) -> Option<(usize, PadControl)> {
        self.bindings
            .iter()
            .find(|&&(_, _, bound)| bound == key)
            .map(|&(player, control, _)| (player, control))
    }

    /// Let `key` work `control` of `player`, besides the control's other
    /// keys. A key works one control, so whatever it worked before it no
    /// longer works.
    pub fn bind(&mut self, player: usize, control: PadControl, key: Key) {
        self.bindings.retain(|&(_, _, bound)| bound != key);
        let entry = (player, control, key);
        let at = self
            .bindings
            .partition_point(|&(p, c, k)| (p, order(c), k) < (player, order(control), key));
        self.bindings.insert(at, entry);
    }

    /// Take every key off `control` of `player`.
    pub fn unbind(&mut self, player: usize, control: PadControl) {
        self.bindings
            .retain(|&(p, c, _)| (p, c) != (player, control));
    }

    /// Every binding, by player, then control (directions, A, B, X, Y, L, R,
    /// Start, Select), then key: for a dialog and for the preferences file.
    pub fn entries(&self) -> impl Iterator<Item = (usize, PadControl, Key)> + '_ {
        self.bindings.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    #[test]
    fn the_defaults_bind_the_documented_keys_to_the_first_player() {
        let defaults = PadBindings::defaults();
        let named: Vec<_> = defaults
            .entries()
            .map(|(player, control, key)| (player, control, keys::name(key).unwrap()))
            .collect();
        assert_eq!(
            named,
            [
                (0, PadControl::Up, "ARROW_UP"),
                (0, PadControl::Down, "ARROW_DOWN"),
                (0, PadControl::Left, "ARROW_LEFT"),
                (0, PadControl::Right, "ARROW_RIGHT"),
                (0, PadControl::A, "X"),
                (0, PadControl::B, "Z"),
                (0, PadControl::X, "S"),
                (0, PadControl::Y, "A"),
                (0, PadControl::L, "Q"),
                (0, PadControl::R, "W"),
                (0, PadControl::Start, "ENTER"),
                (0, PadControl::Select, "SHIFT_RIGHT"),
            ]
        );
        assert_eq!(defaults.control(Key::X), Some((0, PadControl::A)));
        assert_eq!(defaults.control(Key::SHIFT_LEFT), None);
        assert_eq!(PadBindings::default(), defaults);
    }

    #[test]
    fn a_key_works_one_control_and_a_control_takes_several_keys() {
        let mut bindings = PadBindings::defaults();
        bindings.bind(1, PadControl::Start, Key::ENTER);
        assert_eq!(bindings.control(Key::ENTER), Some((1, PadControl::Start)));
        assert!(!bindings
            .entries()
            .any(|entry| entry == (0, PadControl::Start, Key::ENTER)));
        bindings.bind(0, PadControl::A, Key::SPACE);
        assert_eq!(bindings.control(Key::SPACE), Some((0, PadControl::A)));
        assert_eq!(bindings.control(Key::X), Some((0, PadControl::A)));
        bindings.unbind(0, PadControl::A);
        assert_eq!(bindings.control(Key::SPACE), None);
        assert_eq!(bindings.control(Key::X), None);
        assert_eq!(
            bindings.entries().last(),
            Some((1, PadControl::Start, Key::ENTER))
        );
        // The order does not depend on the order of binding.
        let mut reversed = PadBindings::empty();
        for (player, control, key) in bindings.entries().collect::<Vec<_>>().into_iter().rev() {
            reversed.bind(player, control, key);
        }
        assert_eq!(reversed, bindings);
    }
}
