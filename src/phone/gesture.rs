// The injected-event interpreter: the phone display's gesture language.
//
// A remote app injects plain mouse events into the phone display (the
// phone's touchpad logic). This machine re-interprets them the way a phone
// app behaves, per Snug's table:
//
//   phone action              becomes
//   ------------------------  ------------------------------------------
//   drag >= threshold         pixel scrolling (phased, exact taps kept)
//   release while scrolling   inertia by recent speed (capped)
//   tap during inertia        stops the scroll, no click
//   hold 0.5s then drag       a fresh left-down: text selection
//   hold 0.5s then release    right click
//   tap the window's own      intercepted: window restores first, then
//   close button              closes (the app would otherwise save the
//                             phone-display size as its own)
//   everything else           passes through untouched
//
// Positions are phone-display points; thresholds scale with the current
// step width so the feel stays constant across resolution switches. The
// platform backends (macOS CGEventTap, Windows WH_MOUSE_LL) do the event
// filtering - only the remote app's injected events, only inside the phone
// display - and emit the machine's outputs marked so they are never
// re-interpreted.

/// A press held this long is no longer a tap.
pub const HOLD_SECS: f64 = 0.5;
/// Drag distance (design points) that turns a press into scrolling.
pub const SCROLL_START_PTS: f64 = 10.0;
/// Inertia speed cap, points per second.
pub const INERTIA_MAX: f64 = 4000.0;
/// Velocity smoothing factor for the exponential average.
const EMA: f64 = 0.35;
/// A tap within this distance of the press start is a click, not a drag.
const TAP_SLOP_PTS: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
}

/// What the backend should do for one incoming event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Nothing to emit.
    None,
    /// Pass the original event through untouched.
    PassThrough,
    /// Synthesize a fresh left press (selection drag start).
    SynthDown,
    /// Translate movement into scrolling (points, sign = content direction).
    Scroll { dx: f64, dy: f64 },
    /// The drag just ended: kick off inertia at this speed (points/sec).
    Inertia { vx: f64, vy: f64 },
    /// Swallow a tap that only stops an inertia scroll.
    StopOnly,
    /// Tap on the placed window's own close button: restore, then close.
    Close,
    /// Long press released without dragging: the context menu.
    RightClick,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Idle,
    /// Button down, not yet classified as scroll / hold / tap.
    Watching { start: Pos, at: f64, last: Pos, last_t: f64, vx: f64, vy: f64 },
    /// Button down and scrolling.
    Scrolling { last: Pos, last_t: f64, vx: f64, vy: f64 },
    /// Hold recognized, selection-down already synthesized: plain dragging.
    Dragging,
    /// Inertia fly: a tap stops it and is swallowed.
    Flying,
    /// Swallowed the inertia-stopping press, waiting for its release.
    Stopping { at: f64 },
}

#[cfg(windows)]
pub mod win;
#[cfg(target_os = "macos")]
pub mod mac;

/// `monctl phone gesture` - re-interpret the remote app's injected mouse
/// events on the phone display until Ctrl-C. Watch starts and stops this
/// with the session; the standalone command exists for testing.
pub fn run_cli() {
    #[cfg(windows)]
    {
        let Some(area) = crate::phone::window::phone_display_area() else {
            crate::common::fail("phone display is not on (`monctl phone on` first)");
        };
        let Some(fit) = crate::phone::display::win::held_fit_win() else {
            crate::common::fail("phone display is not on");
        };
        let scale = area.bounds.2 / fit.base_width as f64;
        println!("gesture translation on ({:.0}x{:.0} px, {:.1} px/pt) - Ctrl-C to stop", area.bounds.2, area.bounds.3, scale);
        win::run((area.bounds.0, area.bounds.1, area.bounds.2, area.bounds.3), scale);
    }
    #[cfg(target_os = "macos")]
    {
        let Some(area) = crate::phone::window::phone_display_area() else {
            crate::common::fail("phone display is not on (`monctl phone on` first)");
        };
        let Some(fit) = crate::phone::display::mac::held_fit() else {
            crate::common::fail("phone display is not on");
        };
        let scale = area.bounds.2 / fit.base_width as f64;
        println!("gesture translation on ({:.0}x{:.0} pt, {:.1} pt/pt) - Ctrl-C to stop", area.bounds.2, area.bounds.3, scale);
        mac::run((area.bounds.0, area.bounds.1, area.bounds.2, area.bounds.3), scale);
    }
}

pub struct Machine {
    mode: Mode,
    /// Thresholds scale with the current step width (see fit::Fit::scale).
    scale: f64,
    /// The placed window's own close button, if it is on screen.
    close_button: Option<(f64, f64, f64, f64)>,
}

impl Machine {
    pub fn new(scale: f64) -> Machine {
        Machine { mode: Mode::Idle, scale, close_button: None }
    }

    pub fn set_close_button(&mut self, rect: Option<(f64, f64, f64, f64)>) {
        self.close_button = rect;
    }

    fn dist(a: Pos, b: Pos) -> f64 {
        ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
    }

    fn in_close_button(&self, p: Pos) -> bool {
        match self.close_button {
            Some((x, y, w, h)) => p.x >= x && p.x <= x + w && p.y >= y && p.y <= y + h,
            None => false,
        }
    }

    fn smooth(old: f64, new: f64) -> f64 {
        if old == 0.0 {
            return new;
        }
        old * (1.0 - EMA) + new * EMA
    }

    fn capped(v: f64) -> f64 {
        v.clamp(-INERTIA_MAX, INERTIA_MAX)
    }

    pub fn on_down(&mut self, p: Pos, now: f64) -> Gesture {
        match self.mode {
            Mode::Idle => {
                self.mode = Mode::Watching { start: p, at: now, last: p, last_t: now, vx: 0.0, vy: 0.0 };
                Gesture::None
            }
            Mode::Flying => {
                // Stop the scroll; this tap must not click anything.
                self.mode = Mode::Stopping { at: now };
                Gesture::StopOnly
            }
            Mode::Watching { .. } | Mode::Scrolling { .. } | Mode::Dragging | Mode::Stopping { .. } => {
                // Stray press mid-gesture: treat as the end of the old one.
                self.mode = Mode::Watching { start: p, at: now, last: p, last_t: now, vx: 0.0, vy: 0.0 };
                Gesture::None
            }
        }
    }

    pub fn on_move(&mut self, p: Pos, now: f64) -> Gesture {
        match self.mode {
            Mode::Watching { start, at, last, last_t, vx, vy } => {
                let dt = (now - last_t).max(1e-3);
                let nvx = Self::smooth(vx, (p.x - last.x) / dt);
                let nvy = Self::smooth(vy, (p.y - last.y) / dt);
                if Self::dist(p, start) >= SCROLL_START_PTS * self.scale {
                    self.mode = Mode::Scrolling { last: p, last_t: now, vx: nvx, vy: nvy };
                    Gesture::Scroll { dx: p.x - last.x, dy: p.y - last.y }
                } else if now - at >= HOLD_SECS {
                    // Held long enough with little movement: selection drag.
                    self.mode = Mode::Dragging;
                    Gesture::SynthDown
                } else {
                    self.mode = Mode::Watching { start, at, last: p, last_t: now, vx: nvx, vy: nvy };
                    Gesture::None
                }
            }
            Mode::Scrolling { last, last_t, vx, vy } => {
                let dt = (now - last_t).max(1e-3);
                let nvx = Self::smooth(vx, (p.x - last.x) / dt);
                let nvy = Self::smooth(vy, (p.y - last.y) / dt);
                self.mode = Mode::Scrolling { last: p, last_t: now, vx: nvx, vy: nvy };
                Gesture::Scroll { dx: p.x - last.x, dy: p.y - last.y }
            }
            Mode::Dragging => Gesture::PassThrough,
            Mode::Idle | Mode::Flying | Mode::Stopping { .. } => Gesture::PassThrough,
        }
    }

    pub fn on_up(&mut self, p: Pos, now: f64) -> Gesture {
        match self.mode {
            Mode::Watching { start, at, .. } => {
                self.mode = Mode::Idle;
                if Self::dist(p, start) <= TAP_SLOP_PTS * self.scale {
                    if now - at >= HOLD_SECS {
                        Gesture::RightClick
                    } else if self.in_close_button(p) {
                        Gesture::Close
                    } else {
                        Gesture::PassThrough
                    }
                } else {
                    Gesture::PassThrough
                }
            }
            Mode::Scrolling { vx, vy, .. } => {
                self.mode = Mode::Flying;
                Gesture::Inertia { vx: Self::capped(vx), vy: Self::capped(vy) }
            }
            Mode::Dragging => {
                self.mode = Mode::Idle;
                Gesture::PassThrough
            }
            Mode::Stopping { at } => {
                // A sloppy tap can hold for a while; only quick releases
                // stay a "stop", a long press becomes a right click.
                self.mode = Mode::Idle;
                if now - at >= HOLD_SECS {
                    Gesture::RightClick
                } else {
                    Gesture::None
                }
            }
            Mode::Idle | Mode::Flying => Gesture::PassThrough,
        }
    }

    /// The pointer left the display or the session ended mid-gesture.
    pub fn cancel(&mut self) -> Gesture {
        self.mode = Mode::Idle;
        Gesture::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Machine {
        Machine::new(1.0)
    }

    fn p(x: f64, y: f64) -> Pos {
        Pos { x, y }
    }

    #[test]
    fn a_quick_tap_is_a_click() {
        let mut m = machine();
        assert_eq!(m.on_down(p(100.0, 100.0), 0.0), Gesture::None);
        assert_eq!(m.on_up(p(101.0, 101.0), 0.1), Gesture::PassThrough);
    }

    #[test]
    fn dragging_scrolls_with_inertia_on_release() {
        let mut m = machine();
        m.on_down(p(100.0, 100.0), 0.0);
        let g = m.on_move(p(100.0, 160.0), 0.1);
        assert!(matches!(g, Gesture::Scroll { dy, .. } if dy == 60.0));
        let g = m.on_move(p(100.0, 220.0), 0.2);
        assert!(matches!(g, Gesture::Scroll { dy, .. } if dy == 60.0));
        let g = m.on_up(p(100.0, 220.0), 0.25);
        assert!(matches!(g, Gesture::Inertia { vy, .. } if vy > 0.0));
        // The fly continues; a tap stops it and is swallowed.
        assert_eq!(m.on_down(p(100.0, 220.0), 0.4), Gesture::StopOnly);
        assert_eq!(m.on_up(p(100.0, 220.0), 0.45), Gesture::None);
        // Back to normal taps.
        m.on_down(p(50.0, 50.0), 1.0);
        assert_eq!(m.on_up(p(50.0, 50.0), 1.1), Gesture::PassThrough);
    }

    #[test]
    fn long_press_then_release_is_right_click() {
        let mut m = machine();
        m.on_down(p(100.0, 100.0), 0.0);
        assert_eq!(m.on_move(p(100.5, 100.5), 0.2), Gesture::None); // too little, too early
        assert_eq!(m.on_up(p(100.5, 100.5), 0.6), Gesture::RightClick);
    }

    #[test]
    fn long_press_then_drag_selects_text() {
        let mut m = machine();
        m.on_down(p(100.0, 100.0), 0.0);
        assert_eq!(m.on_move(p(101.0, 101.0), 0.55), Gesture::SynthDown);
        assert_eq!(m.on_move(p(120.0, 130.0), 0.7), Gesture::PassThrough);
        assert_eq!(m.on_up(p(140.0, 150.0), 0.9), Gesture::PassThrough);
    }

    #[test]
    fn a_hold_that_ends_in_a_drag_is_not_right_click() {
        let mut m = machine();
        m.on_down(p(100.0, 100.0), 0.0);
        // Cross the scroll threshold right after the hold fired.
        assert_eq!(m.on_move(p(101.0, 101.0), 0.55), Gesture::SynthDown);
        assert_eq!(m.on_up(p(101.0, 101.0), 0.7), Gesture::PassThrough);
    }

    #[test]
    fn the_close_button_tap_is_intercepted() {
        let mut m = machine();
        m.set_close_button(Some((10.0, 10.0, 30.0, 24.0)));
        m.on_down(p(25.0, 20.0), 0.0);
        assert_eq!(m.on_up(p(25.0, 20.0), 0.1), Gesture::Close);
        // Outside the button a tap stays a click.
        m.on_down(p(300.0, 300.0), 1.0);
        assert_eq!(m.on_up(p(300.0, 300.0), 1.1), Gesture::PassThrough);
    }

    #[test]
    fn thresholds_scale_with_the_step_width() {
        let mut m = Machine::new(2.0); // 1080-wide step
        m.on_down(p(100.0, 100.0), 0.0);
        // 15 points at scale 2 is still under the 20-point trigger.
        assert_eq!(m.on_move(p(100.0, 115.0), 0.1), Gesture::None);
        // 21 points crosses it.
        assert!(matches!(m.on_move(p(100.0, 121.0), 0.2), Gesture::Scroll { .. }));
    }

    #[test]
    fn inertia_is_capped_and_a_sloppy_stop_becomes_right_click() {
        let mut m = machine();
        m.on_down(p(100.0, 100.0), 0.0);
        m.on_move(p(100.0, 1100.0), 0.1); // 10000 pt/s raw
        assert!(matches!(m.on_up(p(100.0, 1100.0), 0.1), Gesture::Inertia { vy, .. } if vy == INERTIA_MAX));
        m.on_down(p(100.0, 1100.0), 0.2);
        assert_eq!(m.on_up(p(100.0, 1100.0), 1.0), Gesture::RightClick);
    }

    #[test]
    fn cancel_resets_the_machine() {
        let mut m = machine();
        m.on_down(p(100.0, 100.0), 0.0);
        assert_eq!(m.cancel(), Gesture::None);
        m.on_down(p(50.0, 50.0), 1.0);
        assert_eq!(m.on_up(p(50.0, 50.0), 1.1), Gesture::PassThrough);
    }
}
