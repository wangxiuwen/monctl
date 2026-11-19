// The phone display's lifecycle: create it, retire it, switch its step.
//
// On both platforms the display is ordinary once up: the remote app lists
// it, streams it, and routes injected clicks to it - no encoder involved.
//
// - macOS: a CGVirtualDisplay through virt.rs's objc bridge (the private
//   API DeskPad and Chromium use), every ladder step registered up front
//   with HiDPI on. The process that owns the display must never switch
//   modes itself - doing so delays the display's release by 40s+ (measured
//   by Snug) - so step switches and restoring bumped real displays run in
//   short-lived helper processes (`monctl phone --set-step/--restore-mode`)
//   that use the public CGDisplayConfig API and exit. Lifetime floor:
//   releasing within ~2.5s of creation hangs WindowServer.
// - Windows: one of winvdd's parked pool monitors, claimed at the phone's
//   2x pixel size. The driver's mode table is fixed at device start, so the
//   chosen phone's ladder (sampled, <= 8 entries) is written into the
//   driver settings and learned for the next start; until then the closest
//   same-aspect mode is used.

use super::{Choice, fit::Fit};

/// The screen's name in the OS and in the remote app's display list.
pub const NAME: &str = "monctl 手机屏";
/// macOS display identity: recognizes our own screen; a fixed identity lets
/// the system remember this "monitor's" settings without piling up ICC files.
pub const VENDOR_ID: u32 = 0x7068; // 'ph'
pub const PRODUCT_ID: u32 = 0x0001;
/// First identity that comes online wins; the next build uses the next one.
pub const SERIALS: [u32; 3] = [300_000_101, 300_000_102, 300_000_103];
/// Releasing sooner than this hangs WindowServer (measured by Snug).
pub const MIN_LIFETIME_SECS: f64 = 2.5;
/// Physical density the descriptor reports: phone-like densities make new
/// identities never come online, ~220ppi is the safe choice (measured).
pub const PPI: f64 = 220.0;

pub fn run_cli(args: &[String]) {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("status");
    match cmd {
        "status" => status(),
        "on" => {
            let choice = parse_choice(&args[1..]);
            let fit = choice.fit(super::fit::DEFAULT_INSET);
            super::save_choice(&choice);
            match turn_on(&fit) {
                Ok(msg) => println!("phone display on: {msg}"),
                Err(e) => crate::common::fail(&e),
            }
        }
        "off" => match turn_off() {
            Ok(msg) => println!("phone display off: {msg}"),
            Err(e) => crate::common::fail(&e),
        },
        _ => super::phone_usage(),
    }
}

fn parse_choice(args: &[String]) -> Choice {
    let mut model = None;
    let mut size = None;
    let mut points = None;
    let mut ratio = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--model" => model = it.next().map(String::as_str),
            "--size" => size = it.next().and_then(|v| parse_size(v)),
            "--points" => points = it.next().and_then(|v| v.parse().ok()),
            "--ratio" => ratio = it.next().and_then(|v| v.parse().ok()),
            _ => {}
        }
    }
    // An explicit flag wins; otherwise the phone chosen before; else 540x950.
    Choice::from_flags(model, size, points, ratio)
        .or_else(super::load_choice)
        .unwrap_or(Choice::Ratio(1.759))
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', '*'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The ladder steps the Windows driver's fixed mode table gets: at most 8,
/// evenly sampled from base to widest, as 2x portrait pixels. macOS
/// registers every step; on Windows the table has a hard 50-mode budget.
pub fn sampled_pixels(fit: &Fit) -> Vec<(u32, u32)> {
    const MAX_SAMPLES: usize = 8;
    let steps = fit.widths.len();
    // Sample evenly but count the forced tail (the widest step) too.
    let every = ((steps + MAX_SAMPLES - 2) / (MAX_SAMPLES - 1)).max(1);
    let mut out: Vec<(u32, u32)> = fit.widths.iter().enumerate().filter(|(i, _)| i % every == 0).map(|(_, w)| fit.pixels(*w)).collect();
    let widest = fit.pixels(crate::phone::fit::MAX_WIDTH);
    if out.last() != Some(&widest) {
        out.push(widest);
    }
    out
}

fn status() {
    platform::status();
}

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(target_os = "macos")]
pub use platform::{turn_off, turn_on};

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
use win as platform;
#[cfg(windows)]
pub use platform::{turn_off, turn_on};

#[cfg(not(any(target_os = "macos", windows)))]
fn turn_on(_fit: &Fit) -> Result<String, String> {
    Err(format!("phone display not supported on {}", std::env::consts::OS))
}
#[cfg(not(any(target_os = "macos", windows)))]
fn turn_off() -> Result<String, String> {
    Err(format!("phone display not supported on {}", std::env::consts::OS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampled_pixels_stay_in_budget_and_cover_the_ends() {
        let fit = Fit::for_ratio(1.759);
        let px = sampled_pixels(&fit);
        assert!(px.len() <= 8, "the driver's 50-mode budget is shared with tablets");
        assert_eq!(px[0], fit.pixels(fit.base_width));
        assert_eq!(*px.last().unwrap(), fit.pixels(crate::phone::fit::MAX_WIDTH));
        // Strictly widening, all portrait.
        for pair in px.windows(2) {
            assert!(pair[1].0 > pair[0].0);
            assert!(pair[1].1 > pair[0].1);
        }
    }

    #[test]
    fn sampled_pixels_for_a_short_phone() {
        let fit = Fit::for_ratio(1.293); // iPhone 8-ish, base 620
        let px = sampled_pixels(&fit);
        assert_eq!(px[0], fit.pixels(620));
    }
}
