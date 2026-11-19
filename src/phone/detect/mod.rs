// Remote app detectors: is a phone controlling this machine right now?
//
// monctl never touches a remote app's interfaces - it watches the signals
// each app leaves on this machine (its overlay window, its UIA/AX controller
// list, its stream start/stop in the system log, its TCP peer). Per-app
// readers turn what they see into `Signal`s; the Fusion state machine turns
// signals into one decision, with Snug's timing discipline:
//
// - connect leniently: any signal counts. A controller identified as a
//   computer gets no phone display; several devices at once - any phone or
//   tablet creates one; an unidentified controller is treated as a phone
//   after UNKNOWN_GRACE_SECS.
// - disconnect strictly: signals must stay absent for ABSENCE_SECS (apps
//   rebuild their overlay when switching screens) and QUIET_SECS must have
//   passed since our own last display add/remove (changing the display set
//   makes the app stop and restart its stream, which kills the signals
//   briefly).
// - once disconnected, the display stays RECONNECT_SECS: a reconnect inside
//   that window continues the same session (display and windows kept).
//   Removing only happens when that grace lapses.

#[cfg(windows)]
pub mod probe_win;
#[cfg(target_os = "macos")]
pub mod probe_mac;
#[cfg(target_os = "macos")]
pub mod uu_mac;
#[cfg(windows)]
pub mod uu_win;
#[cfg(windows)]
pub mod todesk_win;
#[cfg(target_os = "macos")]
pub mod todesk_mac;
#[cfg(windows)]
pub mod sunlogin_win;
#[cfg(target_os = "macos")]
pub mod sunlogin_mac;

/// Signals must stay absent this long before we believe a disconnect.
pub const ABSENCE_SECS: f64 = 1.5;
/// After adding or removing the display, ignore signal flapping this long.
pub const QUIET_SECS: f64 = 5.0;
/// A disconnected session keeps its display this long for short drops.
pub const RECONNECT_SECS: f64 = 15.0;
/// An unidentified controller is treated as a phone after this long.
pub const UNKNOWN_GRACE_SECS: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Phone,
    Tablet,
    Computer,
    /// A controller is present but its type is not known.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Signal {
    /// Which app reported it (detector name).
    pub app: &'static str,
    pub device: Device,
}

/// One remote app's signal reader. Implementations poll the machine; they
/// never open the app's own interfaces, only what it leaves on this machine.
pub trait Detector {
    fn name(&self) -> &'static str;
    /// Toolbar room (points per side) this app's on-phone overlay needs.
    fn inset(&self) -> f64;
    fn poll(&mut self) -> Vec<Signal>;
}

/// The platform's detectors, ready to poll.
pub fn registry() -> Vec<Box<dyn Detector>> {
    vec![
        #[cfg(windows)]
        Box::new(uu_win::UuWin),
        #[cfg(target_os = "macos")]
        Box::new(uu_mac::UuMac),
        #[cfg(windows)]
        Box::new(todesk_win::TodeskWin),
        #[cfg(target_os = "macos")]
        Box::new(todesk_mac::TodeskMac),
        #[cfg(windows)]
        Box::new(sunlogin_win::SunloginWin),
        #[cfg(target_os = "macos")]
        Box::new(sunlogin_mac::SunloginMac),
    ]
}

/// What Fusion wants the display layer to do after a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Add,
    Remove,
    /// Keep whatever state we are in.
    Stay,
}

/// Coarse phase, for the tray and the display layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No remote session; no display.
    Idle,
    /// Phone display should be up.
    Active,
    /// Signals quiet; deciding whether the remote really ended.
    Draining,
    /// Disconnected, but the display is kept for a short-drop reconnect.
    Keeping,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Idle,
    /// Waiting UNKNOWN_GRACE_SECS on an unidentified controller.
    AwaitingGrace { since: f64, last_seen: f64 },
    Active,
    Draining { since: f64 },
    Keeping { at: f64 },
}

pub struct Fusion {
    state: State,
    /// Monotonic time of our last display add or remove: the quiet guard.
    last_edge: Option<f64>,
}

impl Fusion {
    pub fn new() -> Fusion {
        Fusion { state: State::Idle, last_edge: None }
    }

    pub fn phase(&self) -> Phase {
        match self.state {
            State::Idle | State::AwaitingGrace { .. } => Phase::Idle,
            State::Active => Phase::Active,
            State::Draining { .. } => Phase::Draining,
            State::Keeping { .. } => Phase::Keeping,
        }
    }

    pub fn awaiting_grace(&self) -> bool {
        matches!(self.state, State::AwaitingGrace { .. })
    }

    /// Seconds left before a Keeping session is torn down, for the tray.
    pub fn keeping_left(&self, now: f64) -> Option<f64> {
        match self.state {
            State::Keeping { at } => Some((RECONNECT_SECS - (now - at)).max(0.0)),
            _ => None,
        }
    }

    fn add(&mut self, now: f64) -> Action {
        self.state = State::Active;
        self.last_edge = Some(now);
        Action::Add
    }

    /// One step of the machine. `now` is monotonic seconds (injected so the
    /// logic is testable); `signals` is what the detectors saw this tick.
    pub fn tick(&mut self, now: f64, signals: &[Signal]) -> Action {
        let seen = !signals.is_empty();
        let computer_only = seen && signals.iter().all(|s| s.device == Device::Computer);
        let phoneish = seen && signals.iter().any(|s| matches!(s.device, Device::Phone | Device::Tablet));
        let guard_ok = !self.last_edge.is_some_and(|t| now - t < QUIET_SECS);

        match self.state {
            State::Idle => {
                if !seen || computer_only {
                    return Action::Stay;
                }
                if phoneish {
                    return self.add(now);
                }
                // Only unidentified controllers: give them a moment.
                let since = match self.state {
                    State::AwaitingGrace { since, .. } => since,
                    _ => now,
                };
                if now - since >= UNKNOWN_GRACE_SECS {
                    return self.add(now);
                }
                self.state = State::AwaitingGrace { since, last_seen: now };
                Action::Stay
            }
            State::AwaitingGrace { since, last_seen } => {
                if !seen {
                    // Signals dropped while deciding: only fresh ones count.
                    if now - last_seen > ABSENCE_SECS {
                        self.state = State::Idle;
                    }
                    return Action::Stay;
                }
                if computer_only {
                    self.state = State::Idle;
                    return Action::Stay;
                }
                if phoneish {
                    return self.add(now);
                }
                if now - since >= UNKNOWN_GRACE_SECS {
                    return self.add(now);
                }
                self.state = State::AwaitingGrace { since, last_seen: now };
                Action::Stay
            }
            State::Active => {
                if seen {
                    return Action::Stay;
                }
                self.state = State::Draining { since: now };
                Action::Stay
            }
            State::Draining { since } => {
                if seen {
                    self.state = State::Active;
                    return Action::Stay;
                }
                if now - since >= ABSENCE_SECS && guard_ok {
                    self.state = State::Keeping { at: now };
                }
                Action::Stay
            }
            State::Keeping { at } => {
                if seen {
                    // Same session: display and windows were never touched.
                    self.state = State::Active;
                    return Action::Stay;
                }
                if now - at >= RECONNECT_SECS {
                    self.state = State::Idle;
                    self.last_edge = Some(now);
                    return Action::Remove;
                }
                Action::Stay
            }
        }
    }
}

impl Default for Fusion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(app: &'static str) -> Signal {
        Signal { app, device: Device::Phone }
    }

    fn computer(app: &'static str) -> Signal {
        Signal { app, device: Device::Computer }
    }

    fn unknown(app: &'static str) -> Signal {
        Signal { app, device: Device::Unknown }
    }

    #[test]
    fn phone_signal_adds_immediately() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu")]), Action::Add);
        assert_eq!(f.phase(), Phase::Active);
    }

    #[test]
    fn computer_only_never_adds() {
        let mut f = Fusion::new();
        for t in [0.0, 1.0, 5.0, 60.0] {
            assert_eq!(f.tick(t, &[computer("uu")]), Action::Stay);
        }
        assert_eq!(f.phase(), Phase::Idle);
    }

    #[test]
    fn unknown_controller_counts_as_phone_after_grace() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[unknown("uu")]), Action::Stay);
        assert!(f.awaiting_grace());
        assert_eq!(f.tick(1.0, &[unknown("uu")]), Action::Stay);
        assert_eq!(f.tick(2.1, &[unknown("uu")]), Action::Add);
    }

    #[test]
    fn mixed_crowd_with_a_phone_adds() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu"), computer("uu")]), Action::Add);
    }

    #[test]
    fn disconnect_needs_absence_then_quiet_guard_then_reconnect_window() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu")]), Action::Add); // edge at 0
        // Absence starts at 1.0. 1.5s of absence is up at 2.5, but the quiet
        // guard (edge 0 + 5s) blocks the disconnect decision until 5.0.
        assert_eq!(f.tick(1.0, &[]), Action::Stay);
        assert_eq!(f.tick(2.4, &[]), Action::Stay);
        assert_eq!(f.tick(5.2, &[]), Action::Stay); // guard ends at 5.0
        assert_eq!(f.tick(5.5, &[]), Action::Stay); // now Keeping, display still up
        assert_eq!(f.phase(), Phase::Keeping);
        // The 15s short-drop window starts at 5.5; removal at 20.5.
        assert_eq!(f.tick(20.0, &[]), Action::Stay);
        assert_eq!(f.tick(20.6, &[]), Action::Remove);
        assert_eq!(f.phase(), Phase::Idle);
    }

    #[test]
    fn reconnect_during_grace_resumes_same_session() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu")]), Action::Add);
        assert_eq!(f.tick(1.0, &[]), Action::Stay);
        assert_eq!(f.tick(5.5, &[]), Action::Stay); // Keeping
        assert_eq!(f.tick(10.0, &[phone("uu")]), Action::Stay);
        assert_eq!(f.phase(), Phase::Active);
        // And it will not remove while the phone is back, ever.
        assert_eq!(f.tick(3600.0, &[phone("uu")]), Action::Stay);
    }

    #[test]
    fn flapping_signals_do_not_trick_a_removal() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu")]), Action::Add);
        // The app rebuilds its overlay: one absent tick, then back.
        assert_eq!(f.tick(6.0, &[]), Action::Stay);
        assert_eq!(f.tick(6.2, &[phone("uu")]), Action::Stay);
        assert_eq!(f.phase(), Phase::Active);
    }

    #[test]
    fn a_fresh_session_after_teardown_adds_again() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu")]), Action::Add);
        assert_eq!(f.tick(1.0, &[]), Action::Stay);
        assert_eq!(f.tick(5.5, &[]), Action::Stay);
        assert_eq!(f.tick(20.6, &[]), Action::Remove); // edge at 20.6
        // A new phone right after removal: the quiet guard defers nothing on
        // the connect side - signals are present, so add again.
        assert_eq!(f.tick(21.0, &[phone("uu")]), Action::Add);
    }

    #[test]
    fn keeping_left_counts_down_for_the_tray() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[phone("uu")]), Action::Add);
        assert_eq!(f.tick(1.0, &[]), Action::Stay);
        assert_eq!(f.tick(5.5, &[]), Action::Stay);
        assert_eq!(f.keeping_left(5.5), Some(15.0));
        assert_eq!(f.keeping_left(13.0), Some(7.5));
        assert_eq!(f.keeping_left(20.6), Some(0.0));
    }

    #[test]
    fn grace_aborts_when_signals_stop() {
        let mut f = Fusion::new();
        assert_eq!(f.tick(0.0, &[unknown("uu")]), Action::Stay);
        assert_eq!(f.tick(0.5, &[]), Action::Stay);
        assert_eq!(f.tick(3.0, &[]), Action::Stay);
        assert_eq!(f.phase(), Phase::Idle);
    }
}
