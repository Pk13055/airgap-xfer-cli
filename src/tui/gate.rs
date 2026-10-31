//! What a keypress means at a confirmation gate.
//!
//! The confirmation model is deliberately lopsided: the sender aims until
//! tracking locks and confirms, and the receiver never confirms anything.
//! These rules keep that model honest — Enter cannot slip through before the
//! camera is locked, the receiver cannot be made to "accept" by key, and a
//! held or bounced Enter that opened one gate cannot also confirm the next.

use std::{
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use crossterm::event::KeyCode;

use crate::tui::optical::GateReply;

/// Enter presses this soon after a gate appears belong to the keypress that
/// ended the previous one (key repeat, a bounce), not to this gate.
pub const ARM_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateSpec {
    /// Enter does nothing until the camera is tracking the peer.
    pub require_lock: bool,
    /// Lock itself is the answer; Enter does nothing at all.
    pub auto_on_lock: bool,
    /// When the gate appeared on screen.
    pub armed_at: Instant,
}

/// A gate the UI is holding open for the protocol thread.
pub struct Gate {
    pub prompt: String,
    pub reply: Sender<GateReply>,
    pub spec: GateSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Proceed,
    Abort,
    Ignore,
}

pub fn on_key(spec: &GateSpec, key: KeyCode, locked: bool, now: Instant) -> KeyAction {
    match key {
        KeyCode::Esc => KeyAction::Abort,
        KeyCode::Enter if spec.auto_on_lock => KeyAction::Ignore,
        KeyCode::Enter if spec.require_lock && !locked => KeyAction::Ignore,
        KeyCode::Enter if now.saturating_duration_since(spec.armed_at) < ARM_DELAY => {
            KeyAction::Ignore
        }
        KeyCode::Enter => KeyAction::Proceed,
        _ => KeyAction::Ignore,
    }
}

/// Whether an auto-on-lock gate should open now.
pub fn auto_proceed(spec: &GateSpec, locked: bool) -> bool {
    spec.auto_on_lock && locked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(require_lock: bool, auto_on_lock: bool, armed_at: Instant) -> GateSpec {
        GateSpec {
            require_lock,
            auto_on_lock,
            armed_at,
        }
    }

    #[test]
    fn enter_is_ignored_until_tracking_locks() {
        let t0 = Instant::now();
        let aim = spec(true, false, t0);
        let later = t0 + ARM_DELAY;
        assert_eq!(
            on_key(&aim, KeyCode::Enter, false, later),
            KeyAction::Ignore
        );
        assert_eq!(
            on_key(&aim, KeyCode::Enter, true, later),
            KeyAction::Proceed
        );
    }

    #[test]
    fn the_receiver_never_confirms_by_key_and_proceeds_on_lock_alone() {
        let t0 = Instant::now();
        let auto = spec(true, true, t0);
        assert_eq!(
            on_key(&auto, KeyCode::Enter, true, t0 + ARM_DELAY),
            KeyAction::Ignore
        );
        assert!(!auto_proceed(&auto, false));
        assert!(auto_proceed(&auto, true));
        assert!(
            !auto_proceed(&spec(true, false, t0), true),
            "the sender must press Enter"
        );
    }

    #[test]
    fn an_enter_that_arrives_with_the_gate_is_not_a_confirmation() {
        let t0 = Instant::now();
        let offer = spec(false, false, t0);
        assert_eq!(
            on_key(
                &offer,
                KeyCode::Enter,
                true,
                t0 + Duration::from_millis(100)
            ),
            KeyAction::Ignore,
            "a held or bounced Enter must not also confirm the next gate"
        );
        assert_eq!(
            on_key(&offer, KeyCode::Enter, true, t0 + ARM_DELAY),
            KeyAction::Proceed
        );
    }

    #[test]
    fn esc_always_aborts_and_other_keys_do_nothing() {
        let t0 = Instant::now();
        for gate in [
            spec(true, false, t0),
            spec(true, true, t0),
            spec(false, false, t0),
        ] {
            assert_eq!(on_key(&gate, KeyCode::Esc, false, t0), KeyAction::Abort);
            assert_eq!(
                on_key(&gate, KeyCode::Char('y'), true, t0 + ARM_DELAY),
                KeyAction::Ignore
            );
        }
    }
}
