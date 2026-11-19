// monctl phone - a phone-shaped portrait display for remote control.
//
// A remote-control app (UU Remote, ToDesk, Sunlogin, ...) squeezes the whole
// desktop onto the phone and everything shrinks several times over. Instead,
// this module adds a dedicated portrait virtual display with the user's
// phone's proportions, moves the window that matters onto it full-screen,
// and re-interprets the remote app's injected mouse events there so the
// phone feels native: one finger scrolls with inertia, a long press opens
// the context menu, long-press-drag selects text. The physical mouse, other
// displays and the remote app's own stream stay untouched.
//
// The design follows Snug (github.com/zephyr4123/snug, MIT) but is remote
// app agnostic: detection is a set of per-app signal readers behind one
// state machine, and the toolbar inset each app needs travels with its
// detector.
//
// Layout:
//   json.rs     tiny JSON parser for bundled data and state files
//   models.rs   bundled phone model database (phone-models.json)
//   fit.rs      resolution ladder math (portrait, 2x HiDPI, per-window steps)
//   display.rs  create/retire the screen: macOS CGVirtualDisplay, Windows VDD
//   detect/     remote app detectors (signals -> connected / disconnected)
//   window.rs   move a window onto the display and back, restore records
//   gesture.rs  the injected-event interpreter (shared state machine)

pub mod detect;
pub mod display;
pub mod fit;
pub mod gesture;
pub mod library;
pub mod json;
pub mod models;
pub mod watch;
pub mod zoom;
pub mod window;

use fit::Fit;

pub fn run(args: &[String]) {
    // One-shot helpers (the macOS holder and mode switches) come first.
    if args.first().is_some_and(|s| s.starts_with("--")) {
        #[cfg(target_os = "macos")]
        display::mac::helper(args);
        #[cfg(not(target_os = "macos"))]
        crate::common::fail("phone display helper processes run on macOS only");
    }
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("status");
    match cmd {
        "models" => models::run_cli(&args[1..]),
        "fit" => fit::run_cli(&args[1..]),
        "on" | "off" | "status" => display::run_cli(args),
        "place" | "restore" | "windows" => window::run_cli(args),
        "watch" => watch::run_cli(&args[1..]),
        "watchd" => watch::run_detached(),
        "detect" => detect_diag(),
        "trust" => trust_cli(),
        "gesture" => gesture::run_cli(),
        "library" => library::run_cli(),
        "-h" | "--help" | "help" => phone_usage(),
        other => {
            let _ = other;
            phone_usage();
        }
    }
}

/// `monctl phone detect` - what each detector sees right now, for
/// calibration: run it while a phone is controlling to see which signals fire.
/// `monctl phone trust` - pop the standard Accessibility permission dialog
/// on the Mac's screen (works over SSH); print the resulting state.
fn trust_cli() {
    #[cfg(target_os = "macos")]
    {
        let ok = window::mac::trust_prompt();
        if ok {
            println!("accessibility: granted");
        } else {
            println!("accessibility: a dialog appeared on the Mac screen - click \"Open System Settings\", then switch the highlighted entry ON and re-run `monctl phone detect`");
        }
    }
    #[cfg(not(target_os = "macos"))]
    crate::common::fail("trust is macOS-only");
}

fn detect_diag() {
    for mut d in detect::registry() {
        let signals = d.poll();
        if signals.is_empty() {
            println!("{:<10} no signal", d.name());
        } else {
            for s in signals {
                println!("{:<10} {:?} ({})", d.name(), s.device, d.inset());
            }
        }
    }
}

fn phone_usage() -> ! {
    print!(
        r#"monctl phone - a phone-shaped portrait display for remote control

usage:
  monctl phone models [QUERY]  list / search the bundled phone models
  monctl phone fit <MODEL | --size WxH | --ratio R> [--inset N] [--points PT]
                               show the resolution ladder a phone gets
  monctl phone on [--model Q | --size WxH | --ratio R] [--remote APP]
                               add the phone display now (default: the phone
                               chosen before, else 540x950)
  monctl phone off             remove the phone display, windows go home
  monctl phone status          what is on, which remote app is connected
  monctl phone windows [Q]     movable windows (Q matches title/app/pid)
  monctl phone place [Q]       move a window onto the phone display (default: the focused one)
  monctl phone restore [Q|all] put placed windows back where they were
  monctl phone gesture         re-interpret injected mouse events on the phone display
                               (scroll with inertia, long press = right click)
  monctl phone library         the app library page on the phone display
  monctl phone watch [--model Q | --size WxH | --ratio R]
                               watch the remote apps (UU Remote, ToDesk, Sunlogin):
                               a phone connecting adds the display + library + gestures,
                               leaving restores windows and removes it
  monctl phone watchd          same, detached from the console (what autostart runs)
"#
    );
    std::process::exit(0);
}

/// The phone choice a session uses: an exact bundled model, a free-form
/// screen size, or a bare ratio - the same three ways Snug lets users
/// describe their phone. Stored in the state file across sessions.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Model(String),
    Size(u32, u32, Option<u32>),
    Ratio(f64),
}

impl Choice {
    pub fn fit(&self, inset: f64) -> Fit {
        match self {
            Choice::Model(q) => match models::lookup(q) {
                Some((_, m)) => Fit::for_screen(m.screen.0, m.screen.1, m.points, inset),
                None => Fit::for_screen(0, 0, None, inset), // caller validated earlier
            },
            Choice::Size(w, h, points) => Fit::for_screen(*w, *h, *points, inset),
            Choice::Ratio(r) => Fit::for_ratio(*r),
        }
    }

    /// Parse CLI model/size/ratio flags into one choice.
    pub fn from_flags(model: Option<&str>, size: Option<(u32, u32)>, points: Option<u32>, ratio: Option<f64>) -> Option<Choice> {
        if let Some(m) = model {
            if crate::phone::models::lookup(m).is_none() {
                crate::common::fail(&format!("no phone model matches \"{}\" - try `monctl phone models {}`", m, m));
            }
            return Some(Choice::Model(m.to_string()));
        }
        if let Some((w, h)) = size {
            return Some(Choice::Size(w, h, points));
        }
        ratio.map(Choice::Ratio)
    }
}

/// The phone choice persists across sessions (`phone on --model` stores it,
/// `watch` and a flagless `phone on` pick it up).
pub fn choice_path() -> std::path::PathBuf {
    crate::common::state_path().with_file_name("phone-choice.json")
}

pub fn save_choice(choice: &Choice) {
    let mut m = crate::common::OffState::new();
    match choice {
        Choice::Model(q) => {
            m.insert("kind".into(), "model".into());
            m.insert("value".into(), q.clone());
        }
        Choice::Size(w, h, points) => {
            m.insert("kind".into(), "size".into());
            m.insert("value".into(), format!("{w}x{h}"));
            if let Some(p) = points {
                m.insert("points".into(), p.to_string());
            }
        }
        Choice::Ratio(r) => {
            m.insert("kind".into(), "ratio".into());
            m.insert("value".into(), r.to_string());
        }
    }
    crate::common::write_atomic(&choice_path(), &crate::common::json_write_object(&m));
}

pub fn load_choice() -> Option<Choice> {
    let m = crate::common::json_parse_object(&std::fs::read_to_string(choice_path()).ok()?)?;
    let (kind, value) = (m.get("kind")?, m.get("value")?);
    match kind.as_str() {
        "model" => Some(Choice::Model(value.clone())),
        "ratio" => value.parse::<f64>().ok().map(Choice::Ratio),
        "size" => {
            let (w, h) = value.split_once('x')?;
            Some(Choice::Size(w.trim().parse().ok()?, h.trim().parse().ok()?, m.get("points").and_then(|p| p.parse().ok())))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_fit_resolves_models_and_sizes() {
        let c = Choice::Model("iPhone 17 Pro Max".into());
        assert_eq!(c.fit(fit::DEFAULT_INSET).size(540), (540, 950));
        assert_eq!(Choice::Size(1080, 2400, None).fit(fit::DEFAULT_INSET).size(540), (540, 954));
        assert_eq!(Choice::Ratio(1.759).fit(fit::DEFAULT_INSET).ratio, 1.759);
    }
}
