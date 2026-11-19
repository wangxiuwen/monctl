// macOS: the phone display is a CGVirtualDisplay with every ladder step
// registered, owned by a detached holder process (the same pattern virt.rs
// uses for generic virtual screens). Nothing about it survives a logout or
// a crashed holder, so a phone display can never get stuck.
//
// Three measured rules shape this file (Snug's field research):
// - the owning process must never switch modes: it would delay the
//   display's release by 40s+. Switches and restores go through short-lived
//   helper processes using the public CGDisplayConfig API.
// - the descriptor reports ~220ppi physical size; phone-like densities make
//   a new identity never come online (and the identity stays blacklisted).
// - releasing within ~2.5s of creation hangs WindowServer.
//
// Layout:
//   holder        `monctl phone --hold <base_w> <ratio> <hz> <serial> <name>`
//                 creates the display, keeps it alive, heartbeats its file
//   set-step      `monctl phone --set-step <id> <w> <h>` - one-shot switch
//   restore-mode  `monctl phone --restore-mode <id> <w> <h> <pw> <ph> <hz>
//                 <ioID> <session|permanent>` - put another display back to
//                 the mode macOS bumped when the screen set changed

use super::{Fit, NAME, PPI, PRODUCT_ID, SERIALS, VENDOR_ID};
use crate::common::{json_parse_object, json_write_object, write_atomic};
use crate::virt::{class, new_mode, ns_string, send, send_bool_id, send_id, send_ret_u32, send_size, send_u32, CGSize};
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::Duration;

type Id = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCFBooleanTrue: Id;
    fn CFArrayCreate(alloc: *const c_void, values: *const Id, n: isize, cb: *const c_void) -> Id;
    fn CFArrayGetCount(a: Id) -> isize;
    fn CFArrayGetValueAtIndex(a: Id, idx: isize) -> *const c_void;
    fn CFDictionaryCreate(alloc: *const c_void, keys: *const c_void, values: *const c_void, n: isize, key_cb: *const c_void, value_cb: *const c_void) -> Id;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGDisplayShowDuplicateLowResolutionModes: Id;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> Id;
    fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayIsAsleep(display: u32) -> bool;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayCopyAllDisplayModes(display: u32, options: *const c_void) -> *mut c_void;
    fn CGDisplayModeGetWidth(m: *const c_void) -> usize;
    fn CGDisplayModeGetHeight(m: *const c_void) -> usize;
    fn CGDisplayModeGetPixelWidth(m: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(m: *const c_void) -> usize;
    fn CGDisplayModeGetRefreshRate(m: *const c_void) -> f64;
    fn CGDisplayModeGetIODisplayModeID(m: *const c_void) -> i32;
    fn CGDisplayModeIsUsableForDesktopGUI(m: *const c_void) -> bool;
    fn CGDisplayModeRelease(m: *mut c_void);
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayWithDisplayMode(config: *mut c_void, display: u32, mode: *const c_void, options: *const c_void) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
}

/// CGConfigureOption: session scope lasts until logout; permanently writes
/// WindowServer's permanent store.
const CONFIGURE_FOR_SESSION: u32 = 1;
const CONFIGURE_PERMANENTLY: u32 = 2;

fn online_ids() -> Vec<u32> {
    unsafe {
        let mut n = 0u32;
        CGGetOnlineDisplayList(0, std::ptr::null_mut(), &mut n);
        let mut ids = vec![0u32; n as usize];
        CGGetOnlineDisplayList(n, ids.as_mut_ptr(), &mut n);
        ids.truncate(n as usize);
        ids
    }
}

/// Options dict asking CGDisplayCopyAllDisplayModes for the 1x/2x variants
/// of every size, not just the desktop-usable defaults.
fn dup_modes_option() -> Id {
    unsafe {
        let keys = &kCGDisplayShowDuplicateLowResolutionModes as *const Id as *const c_void;
        let values = &kCFBooleanTrue as *const Id as *const c_void;
        CFDictionaryCreate(std::ptr::null(), keys, values, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ModeInfo {
    pub width: usize,
    pub height: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub hz: f64,
    pub io_id: i32,
    pub usable: bool,
}

impl ModeInfo {
    unsafe fn from(m: *const c_void) -> ModeInfo {
        ModeInfo {
            width: CGDisplayModeGetWidth(m),
            height: CGDisplayModeGetHeight(m),
            pixel_width: CGDisplayModeGetPixelWidth(m),
            pixel_height: CGDisplayModeGetPixelHeight(m),
            hz: CGDisplayModeGetRefreshRate(m),
            io_id: CGDisplayModeGetIODisplayModeID(m),
            usable: CGDisplayModeIsUsableForDesktopGUI(m),
        }
    }
}

/// Every listed mode of a display with its raw CGDisplayModeRef; the refs
/// stay valid for the life of the returned CFArray (never released here -
/// these run in one-shot helpers, and leaking an array there is harmless).
unsafe fn modes_of(id: u32) -> (Id, Vec<(*const c_void, ModeInfo)>) {
    let arr = CGDisplayCopyAllDisplayModes(id, dup_modes_option());
    if arr.is_null() {
        return (std::ptr::null_mut(), vec![]);
    }
    let n = CFArrayGetCount(arr);
    let out = (0..n).map(|i| {
        let raw = CFArrayGetValueAtIndex(arr, i);
        (raw, ModeInfo::from(raw))
    }).collect();
    (arr, out)
}

/// The display's current mode, or None when it has none (yet / anymore).
fn current(id: u32) -> Option<ModeInfo> {
    unsafe {
        let m = CGDisplayCopyDisplayMode(id);
        if m.is_null() {
            return None;
        }
        let info = ModeInfo::from(m);
        CGDisplayModeRelease(m);
        Some(info)
    }
}

/// The current 2x mode's width in points; None when not at a 2x mode.
fn hidpi_width(id: u32) -> Option<u32> {
    current(id).filter(|m| m.pixel_width == m.width * 2).map(|m| m.width as u32)
}

fn apply_mode(id: u32, raw: *const c_void, scope: u32) -> Result<(), String> {
    unsafe {
        let mut config: *mut c_void = std::ptr::null_mut();
        let begin = CGBeginDisplayConfiguration(&mut config);
        if begin != 0 {
            return Err(format!("CGBeginDisplayConfiguration failed ({begin})"));
        }
        let staged = CGConfigureDisplayWithDisplayMode(config, id, raw, std::ptr::null());
        if staged != 0 {
            CGCancelDisplayConfiguration(config);
            return Err(format!("CGConfigureDisplayWithDisplayMode failed ({staged})"));
        }
        let done = CGCompleteDisplayConfiguration(config, scope);
        if done != 0 {
            CGCancelDisplayConfiguration(config);
            return Err(format!("CGCompleteDisplayConfiguration failed ({done})"));
        }
        Ok(())
    }
}

// ---- creation ------------------------------------------------------------

/// Create the display in this process. It lives exactly as long as the
/// returned objc object; the holder keeps it until its file is removed.
///
/// # Safety
/// Private CoreGraphics classes through the objc runtime; release once.
unsafe fn create_display(fit: &Fit, serial: u32, name: &str, hz: u32) -> Result<(Id, u32), String> {
    for c in ["CGVirtualDisplay", "CGVirtualDisplayDescriptor", "CGVirtualDisplaySettings", "CGVirtualDisplayMode"] {
        if class(c).is_null() {
            return Err(format!("{c} not available on this macOS version (needs macOS 11+)"));
        }
    }
    let max = super::super::fit::MAX_PIXELS;
    let desc = send(send(class("CGVirtualDisplayDescriptor"), "alloc"), "init");
    send_id(desc, "setQueue:", dispatch_get_global_queue(0, 0));
    send_id(desc, "setName:", ns_string(name));
    send_u32(desc, "setMaxPixelsWide:", max.0);
    send_u32(desc, "setMaxPixelsHigh:", max.1);
    // ~220ppi physical size: the density new identities reliably come
    // online at (phone-like ones never do, and stay blacklisted).
    let mm = |px: f64| px / PPI * 25.4;
    send_size(desc, "setSizeInMillimeters:", CGSize { w: mm(max.0 as f64), h: mm(max.1 as f64) });
    send_u32(desc, "setVendorID:", VENDOR_ID);
    send_u32(desc, "setProductID:", PRODUCT_ID);
    send_u32(desc, "setSerialNum:", serial);

    let display = send_id(send(class("CGVirtualDisplay"), "alloc"), "initWithDescriptor:", desc);
    if display.is_null() {
        // A leftover screen with the same serial refuses the build; the
        // caller tries the next identity.
        return Err("WindowServer refused the identity (serial in use?)".into());
    }
    // Every ladder step at once, HiDPI on: step switches then never need a
    // rebuild - and never restart the remote app's stream.
    let modes: Vec<Id> = fit.widths.iter().map(|w| {
        let (mw, mh) = fit.size(*w);
        new_mode(mw, mh, hz as f64)
    }).collect();
    let settings = send(send(class("CGVirtualDisplaySettings"), "alloc"), "init");
    send_u32(settings, "setHiDPI:", 1);
    send_id(settings, "setModes:", CFArrayCreate(std::ptr::null(), modes.as_ptr(), modes.len() as isize, &kCFTypeArrayCallBacks));
    if !send_bool_id(display, "applySettings:", settings) {
        return Err("applySettings failed for the phone ladder".into());
    }
    let id = send_ret_u32(display, "displayID");
    if id == 0 {
        return Err("phone display got no display id".into());
    }
    // Wait until it is online with a mode, so the caller can settle it.
    for _ in 0..50 {
        if online_ids().contains(&id) && current(id).is_some() {
            return Ok((display, id));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("phone display never came online with this identity".into())
}

// ---- state ----------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Held {
    pub serial: u32,
    pub pid: u32,
    pub display_id: u32,
    pub base_width: u32,
    pub ratio: f64,
    pub name: String,
    pub born: u64,
    pub heartbeat: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn file() -> PathBuf {
    crate::common::state_path().with_file_name("phone").join("phone.json")
}

fn write_held(v: &Held) {
    let mut m = crate::common::OffState::new();
    m.insert("serial".into(), v.serial.to_string());
    m.insert("pid".into(), v.pid.to_string());
    m.insert("display_id".into(), v.display_id.to_string());
    m.insert("base_width".into(), v.base_width.to_string());
    m.insert("ratio".into(), v.ratio.to_string());
    m.insert("name".into(), v.name.clone());
    m.insert("born".into(), v.born.to_string());
    m.insert("heartbeat".into(), v.heartbeat.to_string());
    write_atomic(&file(), &json_write_object(&m));
}

fn read_held() -> Option<Held> {
    let m = json_parse_object(&std::fs::read_to_string(file()).ok()?)?;
    let num = |k: &str| m.get(k).and_then(|s| s.parse::<u64>().ok());
    Some(Held {
        serial: num("serial")? as u32,
        pid: num("pid")? as u32,
        display_id: num("display_id")? as u32,
        base_width: num("base_width")? as u32,
        ratio: m.get("ratio")?.parse().ok()?,
        name: m.get("name").cloned().unwrap_or_default(),
        born: num("born")?,
        heartbeat: num("heartbeat")?,
    })
}

fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    pid > 0 && unsafe { kill(pid as i32, 0) } == 0
}

/// The held phone display, if its holder is alive and its file fresh.
fn held() -> Option<Held> {
    let v = read_held()?;
    if now().saturating_sub(v.heartbeat) > 15 || !pid_alive(v.pid) {
        let _ = std::fs::remove_file(file());
        return None;
    }
    Some(v)
}

// ---- holder ----------------------------------------------------------------

fn holder(args: &[String]) -> ! {
    use std::io::Write;
    let get = |i: usize| args.get(i).cloned().unwrap_or_default();
    let fit = Fit::for_ratio(get(1).parse().unwrap_or(1.759));
    let hz: u32 = get(2).parse().unwrap_or(60);
    let serial: u32 = get(3).parse().unwrap_or(SERIALS[0]);
    let name = if get(4).is_empty() { NAME.to_string() } else { get(4) };
    let mut out = std::io::stdout();
    let (_display, display_id) = match unsafe { create_display(&fit, serial, &name, hz) } {
        Ok(x) => x,
        Err(e) => {
            let _ = writeln!(out, "err {e}");
            std::process::exit(1);
        }
    };
    let v = Held { serial, pid: std::process::id(), display_id, base_width: fit.base_width, ratio: fit.ratio, name, born: now(), heartbeat: now() };
    write_held(&v);
    let _ = writeln!(out, "ok {display_id}");
    let _ = out.flush();
    // the CLI only reads the handshake line; nothing else may go to stdout.
    // `display` must stay alive: dropping the objc object drops the screen.
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if !file().exists() {
            std::process::exit(0); // removed: process exit ends the display
        }
        let mut beat = v.clone();
        beat.heartbeat = now();
        write_held(&beat);
    }
}

// ---- service ---------------------------------------------------------------

fn run_helper(args: &[String], timeout: Duration) -> Option<i32> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().ok()?;
    let mut child = Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

/// Switch the phone display to a ladder step (points, 2x). Runs in a helper
/// process: the owning holder must not touch display configuration, or its
/// release takes 40s+.
pub fn set_step(display_id: u32, width_pts: u32, fit: &Fit) -> Result<(), String> {
    let (w, h) = fit.size(width_pts);
    let args = ["phone".to_string(), "--set-step".into(), display_id.to_string(), w.to_string(), h.to_string()];
    let code = run_helper(&args, Duration::from_secs(10)).ok_or("set-step helper did not finish")?;
    if code != 0 {
        return Err(format!("set-step helper failed (exit {code})"));
    }
    // Wait until CoreGraphics agree the switch landed.
    for _ in 0..40 {
        if hidpi_width(display_id) == Some(width_pts) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("set-step to {width_pts}pt did not land within 2s"))
}

/// Add the phone display: try identities until one comes online, then
/// settle it at the base step.
pub fn turn_on(fit: &Fit) -> Result<String, String> {
    if let Some(v) = held() {
        return Err(format!("phone display is already on (display #{}, `monctl phone off` first)", v.display_id));
    }
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    for serial in SERIALS {
        let _ = std::fs::remove_file(file()); // stale state from a crash
        let mut child = Command::new(&exe)
            .args(["phone", "--hold"])
            .arg(fit.base_width.to_string()) // informational; the ladder follows the ratio
            .arg(fit.ratio.to_string())
            .arg("60")
            .arg(serial.to_string())
            .arg(NAME)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0) // survive the terminal that started it
            .spawn()
            .map_err(|e| format!("cannot start holder process: {e}"))?;
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        });
        let line = rx.recv_timeout(Duration::from_secs(15)).unwrap_or_default();
        if let Some(e) = line.trim().strip_prefix("err ") {
            let _ = child.kill();
            eprintln!("monctl: identity {serial} refused ({e}), trying the next");
            continue;
        }
        let Some(id) = line.trim().strip_prefix("ok ").and_then(|s| s.parse::<u32>().ok()) else {
            let _ = child.kill();
            let _ = std::fs::remove_file(file());
            continue;
        };
        // The system first comes online at the widest step it remembers;
        // settle at the base width through the helper.
        if hidpi_width(id) != Some(fit.base_width) {
            if let Err(e) = set_step(id, fit.base_width, fit) {
                let _ = std::fs::remove_file(file());
                let _ = child.kill();
                return Err(format!("phone display #{id} would not settle at {}pt: {e}", fit.base_width));
            }
        }
        let v = read_held().unwrap_or(Held { serial, pid: child.id() as u32, display_id: id, base_width: fit.base_width, ratio: fit.ratio, name: NAME.into(), born: now(), heartbeat: now() });
        let _ = child; // the holder outlives this process on purpose
        return Ok(format!("#{}, \"{}\", {} (ratio {:.3})", v.display_id, v.name, Fit::for_ratio(v.ratio).describe(), v.ratio));
    }
    Err("no identity came online (a leftover screen with one of the serials? try again later, or reboot clears the blacklist)".into())
}

/// Remove the phone display and wait until the system actually drops it.
pub fn turn_off() -> Result<String, String> {
    let Some(v) = held() else { return Ok("phone display is not on".into()) };
    // Respect the WindowServer lifetime floor.
    let age = now().saturating_sub(v.born) as f64;
    if age < super::MIN_LIFETIME_SECS {
        std::thread::sleep(Duration::from_secs_f64(super::MIN_LIFETIME_SECS - age));
    }
    let _ = std::fs::remove_file(file()); // holder exits at its next heartbeat
    // The display disappears when the holder exits (<= 2s). If the displays
    // are asleep (a locked Mac after a remote session), the drop waits for
    // the wake-up - report that instead of blocking for hours.
    let id = v.display_id;
    for _ in 0..60 {
        if !online_ids().contains(&id) {
            return Ok(format!("#{id} retired"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if unsafe { CGDisplayIsAsleep(id) } {
        return Err(format!("#{id} will retire when the display wakes (the Mac is asleep)"));
    }
    Err(format!("#{id} did not go offline within 6s (holder pid {})", v.pid))
}

pub fn status() {
    match held() {
        Some(v) => {
            let step = hidpi_width(v.display_id).map(|w| format!("{w}pt")).unwrap_or_else(|| "no 2x mode".into());
            println!("phone display: #{}, \"{}\", base {}pt (ratio {:.3}), now at {}, holder pid {}", v.display_id, v.name, v.base_width, v.ratio, step, v.pid);
        }
        None => println!("phone display: off"),
    }
    println!("bundled phone models: {} brands", crate::phone::models::brands().len());
}

/// The display id of the held phone display, for the window mover (Phase 2).
pub fn held_display_id() -> Option<u32> {
    held().map(|v| v.display_id)
}

pub fn held_fit() -> Option<Fit> {
    held().map(|v| Fit::for_ratio(v.ratio))
}

// ---- helpers ----------------------------------------------------------------

/// `--set-step <display> <w> <h>`: switch a display to the listed 2x mode
/// of that point size, session scope, exit.
fn set_step_helper(args: &[String]) -> ! {
    use std::io::Write;
    let num = |i: usize| args.get(i).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
    let (id, w, h) = (num(1) as u32, num(2), num(3));
    let mut out = std::io::stdout();
    unsafe {
        let (_arr, modes) = modes_of(id);
        let hit = modes.iter().find(|(_, m)| m.width == w && m.height == h && m.pixel_width == w * 2 && m.usable);
        let Some((raw, _)) = hit else {
            let _ = writeln!(out, "err no 2x mode {w}x{h} on display #{id}");
            std::process::exit(1);
        };
        match apply_mode(id, *raw, CONFIGURE_FOR_SESSION) {
            Ok(()) => {
                let _ = writeln!(out, "ok");
                std::process::exit(0);
            }
            Err(e) => {
                let _ = writeln!(out, "err {e}");
                std::process::exit(1);
            }
        }
    }
}

/// `--restore-mode <display> <w> <h> <pw> <ph> <hz> <ioID> <scope>`: put
/// another display back to the mode it had before the phone display
/// arrived, then exit. Mode numbers reshuffle when the display set changes,
/// so prefer the same number among attribute-equal modes, then any usable
/// one.
fn restore_mode_helper(args: &[String]) -> ! {
    use std::io::Write;
    let num = |i: usize| args.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let id = num(1) as u32;
    let want = ModeInfo { width: num(2) as usize, height: num(3) as usize, pixel_width: num(4) as usize, pixel_height: num(5) as usize, hz: num(6) as f64, io_id: num(7) as i32, usable: true };
    let scope = if args.get(8).map(|s| s == "permanent").unwrap_or(false) { CONFIGURE_PERMANENTLY } else { CONFIGURE_FOR_SESSION };
    let mut out = std::io::stdout();
    unsafe {
        let (_arr, modes) = modes_of(id);
        let same: Vec<(*const c_void, ModeInfo)> = modes.into_iter().filter(|(_, m)| {
            m.width == want.width && m.height == want.height && m.pixel_width == want.pixel_width && m.pixel_height == want.pixel_height && (m.hz - want.hz).abs() < 1.0
        }).collect();
        let hit = same.iter().find(|(_, m)| m.io_id == want.io_id).or_else(|| same.iter().find(|(_, m)| m.usable)).or_else(|| same.first());
        let Some((raw, _)) = hit else {
            let _ = writeln!(out, "err no mode matching {}x{}@{:.0} on display #{id}", want.width, want.height, want.hz);
            std::process::exit(1);
        };
        match apply_mode(id, *raw, scope) {
            Ok(()) => {
                let _ = writeln!(out, "ok");
                std::process::exit(0);
            }
            Err(e) => {
                let _ = writeln!(out, "err {e}");
                std::process::exit(1);
            }
        }
    }
}

/// One-shot helper entry: `monctl phone --<helper> ...`, never returns.
pub fn helper(args: &[String]) -> ! {
    match args.first().map(|s| s.as_str()) {
        Some("--hold") => holder(&args[1..]),
        Some("--set-step") => set_step_helper(args),
        Some("--restore-mode") => restore_mode_helper(args),
        other => crate::common::fail(&format!("unknown phone helper {other:?}")),
    }
}
