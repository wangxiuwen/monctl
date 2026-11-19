// Windows: the phone display is one of winvdd's parked pool monitors,
// claimed at the phone's 2x pixel size.
//
// The driver's mode table is fixed at device start, so `turn_on` first
// learns the chosen phone's sampled ladder (next device start offers it
// exactly; until then `claim` picks the closest same-aspect mode) and then
// claims a free pool monitor. Parking and claiming need no admin rights -
// only the driver install does, and winvdd::ensure handles that.

use super::{Fit, sampled_pixels};
use crate::winvdd;

fn state_path() -> std::path::PathBuf {
    crate::common::state_path().with_file_name("phone-win.json")
}

/// What a claimed phone monitor looks like, for `off` and `status`.
#[derive(Clone, Debug)]
pub struct Claimed {
    /// The monitor's `\\.\DISPLAYn` name.
    pub monitor: String,
    pub width: u32,
    pub height: u32,
    /// Points the pixels represent (pixels are 2x).
    pub base_width: u32,
    pub ratio: f64,
}

fn load_claimed() -> Option<Claimed> {
    let m = crate::common::json_parse_object(&std::fs::read_to_string(state_path()).ok()?)?;
    Some(Claimed {
        monitor: m.get("monitor")?.clone(),
        width: m.get("width")?.parse().ok()?,
        height: m.get("height")?.parse().ok()?,
        base_width: m.get("base_width")?.parse().ok()?,
        ratio: m.get("ratio")?.parse().ok()?,
    })
}

fn save_claimed(c: &Claimed) {
    let mut m = crate::common::OffState::new();
    m.insert("monitor".into(), c.monitor.clone());
    m.insert("width".into(), c.width.to_string());
    m.insert("height".into(), c.height.to_string());
    m.insert("base_width".into(), c.base_width.to_string());
    m.insert("ratio".into(), c.ratio.to_string());
    crate::common::write_atomic(&state_path(), &crate::common::json_write_object(&m));
}

fn clear_claimed() {
    let _ = std::fs::remove_file(state_path());
}

/// Add the phone display: driver up, ladder learned, a free pool monitor
/// claimed at the phone's base step. Returns a human-readable line.
pub fn turn_on(fit: &Fit) -> Result<String, String> {
    winvdd::ensure()?;
    // The device's mode table is fixed at start: learn the ladder so the
    // next start offers these exact sizes. claim() falls back to the
    // closest same-aspect mode until then, and both keep the aspect ratio,
    // so the stream never distorts.
    for &(w, h) in sampled_pixels(fit).iter() {
        winvdd::learn_size(w, h);
    }
    let busy = winvdd::displays();
    let Some(free) = winvdd::pool().into_iter().find(|m| !busy.contains(m)) else {
        return Err("the virtual display pool has no free monitor - another tablet may be using them all (`monctl driver status`)".into());
    };
    let (pw, ph) = fit.pixels(fit.base_width);
    let (w, h) = winvdd::claim(&free, pw, ph)?;
    let claimed = Claimed { monitor: free, width: w, height: h, base_width: fit.base_width, ratio: fit.ratio };
    save_claimed(&claimed);
    Ok(format!("{} at {}x{} px (base {}pt, ratio {:.3})", claimed.monitor, w, h, claimed.base_width, claimed.ratio))
}

/// Remove the phone display: back to the parked pool.
pub fn turn_off() -> Result<String, String> {
    let Some(c) = load_claimed() else {
        // Nothing recorded - sweep any stray pool monitors off the desktop.
        winvdd::park();
        return Ok("nothing claimed (pool parked)".into());
    };
    crate::windows::set_enabled(&c.monitor, false).map_err(|e| format!("cannot park {}: {e}", c.monitor))?;
    clear_claimed();
    Ok(format!("{} parked", c.monitor))
}

/// Switch the phone display to another ladder step (2x pixels). The driver
/// only offers modes from its startup table, so this snaps to the closest
/// sampled step.
pub fn set_step(width_pts: u32, fit: &Fit) -> Result<(u32, u32), String> {
    let Some(c) = load_claimed() else { return Err("phone display is not on".into()) };
    let (pw, ph) = fit.pixels(width_pts);
    let size = winvdd::claim(&c.monitor, pw, ph)?;
    Ok(size)
}

pub fn status() {
    match load_claimed() {
        Some(c) => {
            let on = winvdd::displays().contains(&c.monitor);
            println!("phone display: {} at {}x{} px{} ({:.0}x{:.0} pt at 2x)", c.monitor, c.width, c.height, if on { "" } else { " (recorded, monitor not on the desktop)" }, c.width as f64 / 2.0, c.height as f64 / 2.0);
        }
        None => println!("phone display: off"),
    }
    let busy = winvdd::displays();
    for m in winvdd::pool() {
        println!("  pool {}  {}", m, if busy.contains(&m) { "on the desktop" } else { "parked" });
    }
}

/// The claimed monitor's name, for the window mover (Phase 2).
pub fn claimed_monitor() -> Option<String> {
    load_claimed().map(|c| c.monitor)
}

/// The fit of the claimed phone display, from its recorded ratio.
pub fn held_fit_win() -> Option<super::Fit> {
    load_claimed().map(|c| super::Fit::for_ratio(c.ratio))
}
