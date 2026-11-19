// Moving windows onto the phone display and back, with restore records.
//
// Before a window is touched, where it was (frame, fullscreen, minimized,
// font zoom steps) and the size we gave it are written to the state file;
// the record is deleted only after a successful restore, and if the record
// cannot be written the window must not be moved at all. If monctl dies
// mid-session, the next start (or the next placement) restores from these
// records first - a window must never keep the phone display's size on the
// user's own screen.
//
// The records also cover windows closed elsewhere: macOS will not let us
// intercept another app closing its window, and by the time we notice, the
// app has likely saved the phone-display size as the window's own. Such
// records are kept as "left behind" for two weeks; if the same app opens a
// window again at exactly the size we gave it, the app carried the
// placement through, and the window goes back to its recorded place.
//
// Platform movers (AX on macOS, SetWindowPos + DWM borders on Windows) sit
// behind the cfg-selected backend below; this file is records and policy.

use super::json::Json;
use super::fit::Fit;
use std::collections::BTreeMap;

/// Identify a window across calls: process id + platform window number.
pub type WindowId = String;

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
use win as platform;

/// One movable window, as the movers report it.
#[derive(Clone, Debug)]
pub struct Movable {
    pub id: WindowId,
    pub pid: u32,
    pub title: String,
    pub app: String,
    pub frame: (f64, f64, f64, f64),
    pub minimized: bool,
    pub fullscreen: bool,
}

/// The phone display's geometry the mover needs, points on macOS and 2x
/// pixels on Windows (the coordinate space that platform moves in).
#[derive(Clone, Copy, Debug)]
pub struct DisplayArea {
    /// The whole display.
    pub bounds: (f64, f64, f64, f64),
    /// Bounds minus the system's own chrome (menu bar / taskbar); windows
    /// may fill all of it. Window frames use the same top-left-origin
    /// global coordinates.
    pub usable: (f64, f64, f64, f64),
}

/// The window's target frame at the current step: fill the usable area and
/// leave the home indicator strip free. `current_pts` is the step the
/// display is at now; on macOS the display space is points (scale 1), on
/// Windows it is 2x pixels (scale 2).
pub fn target_frame(area: &DisplayArea, current_pts: u32) -> (f64, f64, f64, f64) {
    let scale = area.bounds.2 / f64::from(current_pts).max(1.0);
    let bar = f64::from(Fit::home_bar_height(f64::from(current_pts))) * scale;
    (area.usable.0, area.usable.1, area.usable.2, (area.usable.3 - bar).max(0.0))
}

// ---- CLI ----------------------------------------------------------------

pub fn run_cli(args: &[String]) {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("windows");
    let query = args[1..].join(" ");
    match cmd {
        "windows" => list_windows_cli(&query),
        "place" => place_cli(&query),
        "restore" => restore_cli(&query),
        _ => crate::common::fail("usage: monctl phone place|restore|windows [QUERY]"),
    }
}

fn list_windows_cli(query: &str) {
    let wins = platform::list_windows();
    let ql = query.to_lowercase();
    let mut n = 0;
    for w in &wins {
        if !ql.is_empty() && !w.title.to_lowercase().contains(&ql) && !w.app.to_lowercase().contains(&ql) && w.pid.to_string() != query.trim() {
            continue;
        }
        println!("{}  pid {}  {:4}x{:<4} at ({},{})  {}", w.id, w.pid, w.frame.2 as i64, w.frame.3 as i64, w.frame.0 as i64, w.frame.1 as i64, w.title);
        n += 1;
    }
    println!("- {} windows{}", n, if ql.is_empty() { String::new() } else { format!(" matching \"{query}\"") });
}

fn place_cli(query: &str) {
    let Some(area) = platform::phone_display_area() else { crate::common::fail("phone display is not on (`monctl phone on` first)") };
    let fit = match platform::held_fit() {
        Some(f) => f,
        None => crate::common::fail("phone display is not on (`monctl phone on` first)"),
    };
    let target = pick_target(query, &area);
    let win = target.ok_or_else(|| format!("no window matches \"{query}\"")).unwrap_or_else(|e| crate::common::fail(&e));
    let placed = place_window(win, &fit, area);
    println!("placed: {placed}");
}

fn pick_target(query: &str, _area: &DisplayArea) -> Option<Movable> {
    let wins = platform::list_windows();
    let q = query.trim();
    if q.is_empty() {
        return platform::focused_window();
    }
    let ql = q.to_lowercase();
    let by_pid = wins.iter().find(|w| w.pid.to_string() == q).cloned();
    let by_title = wins.iter().find(|w| w.title.to_lowercase().contains(&ql)).cloned();
    let by_app = wins.iter().find(|w| w.app.to_lowercase().contains(&ql)).cloned();
    by_pid.or(by_title).or(by_app)
}

/// Record, fill, read back, widen if the app refuses to shrink, refill.
/// Returns a human line.
pub fn place_window(mut win: Movable, fit: &Fit, mut area: DisplayArea) -> String {
    // Returns a human line on success, an explanation on failure; never exits
    // (the library calls this from inside the watch process).
    if win.minimized {
        platform::set_minimized(&win.id, false);
        win = platform::refresh(&win.id).unwrap_or(win);
    }
    if win.fullscreen {
        platform::set_fullscreen(&win.id, false);
        win = platform::refresh(&win.id).unwrap_or(win);
    }
    let mut records = Records::load();
    if !records.place(&win.id, RestoreRecord {
        frame: win.frame,
        placed_size: (0.0, 0.0),
        fullscreen: win.fullscreen,
        minimized: win.minimized,
        zoom: 0,
        placed_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    }) {
        return "cannot persist the restore record - the window was left alone".to_string();
    }
    let mut current = fit.base_width;
    for _ in 0..3 {
        let frame = target_frame(&area, current);
        platform::set_frame(&win.id, frame);
        std::thread::sleep(std::time::Duration::from_millis(350));
        let back = platform::refresh(&win.id).map(|m| (m.frame.2, m.frame.3)).unwrap_or((frame.2, frame.3));
        let scale = area.bounds.2 / f64::from(current).max(1.0);
        let usable = (area.usable.2, area.usable.3 - f64::from(Fit::home_bar_height(f64::from(current))) * scale);
        let next = fit.width_to_fit(back, usable, current);
        if next == current {
            // Settled. The record stays until restore/teardown; only the
            // carried-through size is updated for left-behind matching.
            if let Some(r) = records.placed.get_mut(&win.id) {
                r.placed_size = back;
            }
            records.save();
            // Zoom to the app's remembered level (eligibility-checked).
            let level = super::zoom::level_of(&win.app);
            if level != 0 {
                platform::focus(&win.id);
                super::zoom::zoom(&win.app, level);
            }
            return format!("\"{}\" filled at {}pt ({}x{}, zoom {})", win.title, current, back.0 as i64, back.1 as i64, level);
        }
        // The app would not shrink that far: widen the display and refill.
        match platform::set_step_width(next) {
            Ok(achieved) => {
                current = achieved;
                match platform::phone_display_area() {
                    Some(a) => area = a,
                    None => break,
                }
            }
            Err(_) => break,
        }
    }
    format!("\"{}\" placed at {}pt (widest usable step)", win.title, current)
}

/// Put windows back. `all` or a query; the phone display may already be off.
fn restore_cli(query: &str) {
    let mut records = Records::load();
    let q = query.trim();
    let ids: Vec<WindowId> = if q.is_empty() || q == "all" {
        records.placed.keys().cloned().collect()
    } else {
        let ql = q.to_lowercase();
        records.placed.keys().filter(|id| {
            platform::refresh(id).map(|w| w.title.to_lowercase().contains(&ql)).unwrap_or(false)
                || id.to_lowercase().contains(&ql)
        }).cloned().collect()
    };
    if ids.is_empty() {
        crate::common::fail("no placed windows to restore (`monctl phone place` puts some there)");
    }
    let mut restored = 0;
    for id in ids {
        match restore_one(id.clone(), &mut records) {
            Ok(()) => restored += 1,
            Err(e) => println!("monctl: {e}"),
        }
    }
    println!("- {restored} window(s) back in place");
}

fn restore_one(id: WindowId, records: &mut Records) -> Result<(), String> {
    let Some(r) = records.placed.get(&id).cloned() else { return Err(format!("{id}: no record")) };
    match platform::refresh(&id) {
        Some(w) => {
            let _ = &w;
            if r.minimized {
                platform::set_minimized(&id, false);
            }
            if w.fullscreen {
                platform::set_fullscreen(&id, false);
            }
            platform::set_frame(&id, r.frame);
            if r.minimized {
                platform::set_minimized(&id, true);
            }
            if r.zoom != 0 {
                platform::focus(&id);
                super::zoom::zoom(&w.app, -r.zoom);
            }
            records.restored(&id);
            Ok(())
        }
        None => {
            // Closed while placed: keep the record for carry-through matching.
            records.vanished(&id, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
            Err(format!("{id}: window is gone, record kept as left behind"))
        }
    }
}

/// The phone display's geometry, from the platform mover.
pub fn phone_display_area() -> Option<DisplayArea> {
    platform::phone_display_area()
}

/// Open an app from the library and land its window on the phone display -
/// the whole point of the screen, no dragging involved. If the app was
/// already running and no new window appears, its existing window is moved
/// instead: tapping an entry always ends with something filling the phone.
pub fn open_and_place(catalog: &impl crate::phone::library::Catalog, name: &str) -> Result<String, String> {
    let before: std::collections::HashSet<WindowId> = platform::list_windows().into_iter().map(|w| w.id).collect();
    catalog.open(name)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Some(win) = platform::list_windows().into_iter().find(|w| !before.contains(&w.id)) {
            return finish_place(win, name);
        }
    }
    // No new window: the app was already running - grab its existing window.
    let ql = name.to_lowercase();
    let Some(win) = platform::list_windows().into_iter().find(|w| w.app.to_lowercase().contains(&ql) || w.title.to_lowercase().contains(&ql)) else {
        return Err(format!("{name} opened but no window appeared"));
    };
    finish_place(win, name)
}

fn finish_place(win: Movable, name: &str) -> Result<String, String> {
    let area = phone_display_area().ok_or("phone display is not on")?;
    let fit = platform::held_fit().ok_or("phone display is not on")?;
    // Remember the level for next time: first placement uses the default.
    Ok(place_window(win, &fit, area))
}

/// Restore everything that is still placed - the watch loop and teardown use it.
pub fn restore_all_placed() -> usize {
    let mut records = Records::load();
    let ids: Vec<WindowId> = records.placed.keys().cloned().collect();
    let mut n = 0;
    for id in ids {
        if restore_one(id, &mut records).is_ok() {
            n += 1;
        }
    }
    n
}

#[derive(Clone, Debug, PartialEq)]
pub struct RestoreRecord {
    /// Frame the window had before it was placed, in global points
    /// (top-left origin, matching both AX and CGDisplayBounds).
    pub frame: (f64, f64, f64, f64),
    /// Size (points) the window had while placed, for left-behind matching.
    pub placed_size: (f64, f64),
    pub fullscreen: bool,
    pub minimized: bool,
    /// Font zoom steps applied while it was placed (-4..=8).
    pub zoom: i32,
    /// When it was placed (unix seconds), for left-behind expiry.
    pub placed_at: u64,
}

fn num(x: f64) -> Json {
    Json::Num(x)
}

fn to_json(id: &WindowId, r: &RestoreRecord, left_behind: bool) -> Json {
    Json::Obj(vec![
        ("id".into(), Json::Str(id.clone())),
        ("frame".into(), Json::Arr(vec![num(r.frame.0), num(r.frame.1), num(r.frame.2), num(r.frame.3)])),
        ("placed_size".into(), Json::Arr(vec![num(r.placed_size.0), num(r.placed_size.1)])),
        ("fullscreen".into(), Json::Bool(r.fullscreen)),
        ("minimized".into(), Json::Bool(r.minimized)),
        ("zoom".into(), Json::Num(f64::from(r.zoom))),
        ("placed_at".into(), Json::Num(r.placed_at as f64)),
        ("left_behind".into(), Json::Bool(left_behind)),
    ])
}

fn from_json(v: &Json) -> Option<(WindowId, RestoreRecord, bool)> {
    let id = v.get("id").and_then(Json::as_str)?.to_string();
    let frame = v.get("frame").and_then(Json::as_arr)?;
    if frame.len() != 4 {
        return None;
    }
    let f: Vec<f64> = frame.iter().filter_map(Json::as_num).collect();
    let ps = v.get("placed_size").and_then(Json::as_arr)?;
    if ps.len() != 2 {
        return None;
    }
    let p: Vec<f64> = ps.iter().filter_map(Json::as_num).collect();
    let record = RestoreRecord {
        frame: (f[0], f[1], f[2], f[3]),
        placed_size: (p[0], p[1]),
        fullscreen: v.get("fullscreen").and_then(Json::as_bool).unwrap_or(false),
        minimized: v.get("minimized").and_then(Json::as_bool).unwrap_or(false),
        zoom: v.get("zoom").and_then(Json::as_num).unwrap_or(0.0) as i32,
        placed_at: v.get("placed_at").and_then(Json::as_num).unwrap_or(0.0) as u64,
    };
    Some((id, record, v.get("left_behind").and_then(Json::as_bool).unwrap_or(false)))
}

/// The whole state file: windows currently placed, plus left-behind ones.
#[derive(Default)]
pub struct Records {
    pub placed: BTreeMap<WindowId, RestoreRecord>,
    pub left: BTreeMap<WindowId, RestoreRecord>,
}

pub fn state_path() -> std::path::PathBuf {
    crate::common::state_path().with_file_name("phone-windows.json")
}

impl Records {
    pub fn load() -> Records {
        Records::load_from(&state_path())
    }

    pub fn load_from(path: &std::path::Path) -> Records {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Records::default(),
        };
        Records::from_text(&text)
    }

    pub fn from_text(text: &str) -> Records {
        let v = match super::json::parse(text) {
            Ok(v) => v,
            Err(_) => return Records::default(),
        };
        let mut records = Records::default();
        for row in v.get("placed").and_then(Json::as_arr).unwrap_or(&[]) {
            if let Some((id, r, _)) = from_json(row) {
                records.placed.insert(id, r);
            }
        }
        for row in v.get("left").and_then(Json::as_arr).unwrap_or(&[]) {
            if let Some((id, r, _)) = from_json(row) {
                records.left.insert(id, r);
            }
        }
        records
    }

    fn save(&self) {
        self.save_to(&state_path());
    }

    pub fn save_to(&self, path: &std::path::Path) {
        crate::common::write_atomic(path, &self.to_text());
    }

    /// The file's text; separated from IO so tests exercise it directly.
    pub fn to_text(&self) -> String {
        let placed: Vec<Json> = self.placed.iter().map(|(id, r)| to_json(id, r, false)).collect();
        let left: Vec<Json> = self.left.iter().map(|(id, r)| to_json(id, r, true)).collect();
        format!("{{\"placed\":{},\"left\":{}}}", Json::Arr(placed).write(), Json::Arr(left).write())
    }

    /// Record before moving a window. Returns false if the file did not
    /// persist - the caller must then leave the window where it was.
    pub fn place(&mut self, id: &str, record: RestoreRecord) -> bool {
        self.placed.insert(id.to_string(), record);
        self.save();
        Records::load().placed.contains_key(id)
    }

    /// A successful restore: forget the window. Left-behind records persist.
    pub fn restored(&mut self, id: &str) {
        self.placed.remove(id);
        self.save();
    }

    /// The window vanished while placed: keep its record as left behind.
    pub fn vanished(&mut self, id: &str, now: u64) {
        if let Some(r) = self.placed.remove(id) {
            self.left.insert(id.to_string(), r);
        }
        self.expire_left(now);
    }

    /// Left-behind records expire after two weeks.
    pub fn expire_left(&mut self, now: u64) {
        let two_weeks: u64 = 14 * 24 * 3600;
        let before = self.left.len();
        self.left.retain(|_, r| now.saturating_sub(r.placed_at) < two_weeks);
        if self.left.len() != before {
            self.save();
        }
    }

    /// A window reappeared. If its size equals exactly what we had given it
    /// while placed, the app saved and carried the placement through: hand
    /// back the record so the caller puts it in its old place (and shrinks
    /// the zoom back). A size the user or app changed is left alone.
    pub fn reappeared(&self, id: &str, seen: (f64, f64)) -> Option<RestoreRecord> {
        let r = self.left.get(id)?;
        let tol = 1.0;
        if (seen.0 - r.placed_size.0).abs() <= tol && (seen.1 - r.placed_size.1).abs() <= tol {
            Some(r.clone())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> RestoreRecord {
        RestoreRecord { frame: (100.0, 200.0, 800.0, 600.0), placed_size: (540.0, 950.0), fullscreen: false, minimized: false, zoom: 3, placed_at: 1_000 }
    }

    /// A Records with one placed window, built without touching the disk.
    fn with_placed() -> Records {
        let mut r = Records::default();
        r.placed.insert("w1".to_string(), record());
        r
    }

    #[test]
    fn records_roundtrip_through_json_text() {
        let r = with_placed();
        let loaded = Records::from_text(&r.to_text());
        assert_eq!(loaded.placed.get("w1"), Some(&record()));
    }

    #[test]
    fn save_load_roundtrip_uses_a_real_file() {
        let path = std::env::temp_dir().join(format!("monctl-phone-test-{}.json", std::process::id()));
        let r = with_placed();
        r.save_to(&path);
        let loaded = Records::load_from(&path);
        assert_eq!(loaded.placed.get("w1"), Some(&record()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn place_requires_the_record_to_persist() {
        // The real safety check: place() verifies the write by reading back.
        let mut r = with_placed();
        r.save(); // set the stage through the same IO path place() uses
        assert!(r.place("w2", record()));
        assert_eq!(Records::load().placed.get("w2"), Some(&record()));
        Records::default().save(); // clean up the real state file
    }

    #[test]
    fn vanished_becomes_left_behind_and_expires() {
        let mut r = with_placed();
        r.vanished("w1", 1_000);
        assert!(r.placed.is_empty());
        assert_eq!(r.left.len(), 1);
        r.expire_left(1_000 + 14 * 24 * 3600 + 1);
        assert!(r.left.is_empty());
    }

    #[test]
    fn restore_does_not_touch_left_behind() {
        let mut r = with_placed();
        r.vanished("w1", 1_000);
        r.restored("w1");
        assert_eq!(r.left.len(), 1, "left-behind records survive a restore call");
    }

    #[test]
    fn reappeared_matches_only_carry_through_sizes() {
        let mut r = with_placed();
        r.vanished("w1", 1_000);
        // Same size the window had while placed: the app carried it through.
        assert!(r.reappeared("w1", (540.0, 950.0)).is_some());
        assert!(r.reappeared("w1", (540.5, 950.4)).is_some());
        // Anything else means the user or app changed it: leave it alone.
        assert!(r.reappeared("w1", (1280.0, 800.0)).is_none());
        assert!(r.reappeared("missing", (540.0, 950.0)).is_none());
    }
}
