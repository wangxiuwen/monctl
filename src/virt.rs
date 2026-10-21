// macOS virtual displays — CGVirtualDisplay, the private CoreGraphics class
// behind DeskPad, BetterDisplay's virtual screens and Chromium's display
// tests. Called through the Objective-C runtime directly (no deps).
//
// A virtual display lives exactly as long as the object that created it, so
// each one is owned by a tiny detached holder process (`monctl virtual
// --hold ...`). Removing it = ending that process. Nothing outlives a
// logout/reboot or a crashed holder, so a virtual screen can never get stuck.

use crate::common::{fail, json_parse_object, json_write_object, write_atomic};
use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CString};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFArrayCreate(alloc: *const c_void, values: *const Id, n: isize, cb: *const c_void) -> Id;
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> Id;
}

extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> Id;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayCopyDisplayMode(d: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(m: *const c_void) -> usize;
    fn CGDisplayModeRelease(m: *mut c_void);
}

fn has_mode(id: u32) -> bool {
    unsafe {
        let m = CGDisplayCopyDisplayMode(id);
        if m.is_null() {
            return false;
        }
        let ok = CGDisplayModeGetPixelWidth(m) > 0;
        CGDisplayModeRelease(m);
        ok
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: f64,
    h: f64,
}

// Vendor/product make macOS treat each serial as one stable "monitor", so
// arrangement and chosen resolution are remembered per virtual screen.
const VENDOR_ID: u32 = 0x6d6e; // "mn"
const PRODUCT_ID: u32 = 0x7663; // "vc"
const HEARTBEAT_SECS: u64 = 2;
const STALE_AFTER_SECS: u64 = 15;

fn sel(name: &str) -> Sel {
    let c = CString::new(name).unwrap();
    unsafe { sel_registerName(c.as_ptr()) }
}

fn class(name: &str) -> Id {
    let c = CString::new(name).unwrap();
    unsafe { objc_getClass(c.as_ptr()) }
}

// objc_msgSend must be called through a pointer of the exact method signature.
fn msg_fn() -> *const c_void {
    objc_msgSend as unsafe extern "C" fn() as *const c_void
}

unsafe fn send(obj: Id, s: &str) -> Id {
    let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(msg_fn());
    f(obj, sel(s))
}

unsafe fn send_id(obj: Id, s: &str, a: Id) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(msg_fn());
    f(obj, sel(s), a)
}

unsafe fn send_u32(obj: Id, s: &str, a: u32) {
    let f: unsafe extern "C" fn(Id, Sel, u32) = std::mem::transmute(msg_fn());
    f(obj, sel(s), a)
}

unsafe fn send_size(obj: Id, s: &str, a: CGSize) {
    let f: unsafe extern "C" fn(Id, Sel, CGSize) = std::mem::transmute(msg_fn());
    f(obj, sel(s), a)
}

unsafe fn send_bool_id(obj: Id, s: &str, a: Id) -> bool {
    let f: unsafe extern "C" fn(Id, Sel, Id) -> bool = std::mem::transmute(msg_fn());
    f(obj, sel(s), a)
}

unsafe fn send_ret_u32(obj: Id, s: &str) -> u32 {
    let f: unsafe extern "C" fn(Id, Sel) -> u32 = std::mem::transmute(msg_fn());
    f(obj, sel(s))
}

unsafe fn new_mode(w: u32, h: u32, hz: f64) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, u32, u32, f64) -> Id = std::mem::transmute(msg_fn());
    f(send(class("CGVirtualDisplayMode"), "alloc"), sel("initWithWidth:height:refreshRate:"), w, h, hz)
}

unsafe fn ns_string(s: &str) -> Id {
    let c = CString::new(s.replace('\0', "")).unwrap();
    CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), 0x0800_0100) // toll-free bridged to NSString
}

// ---- spec ----

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub hidpi: bool,
}

/// "2560x1440", "1920x1080@120", "4k", "1080p"...
pub fn parse_size(s: &str) -> Option<(u32, u32, Option<u32>)> {
    let (size, hz) = match s.split_once('@') {
        Some((a, b)) => (a, Some(b.trim_end_matches("Hz").trim_end_matches("hz").parse().ok()?)),
        None => (s, None),
    };
    let (w, h) = match size.to_ascii_lowercase().as_str() {
        "720p" => (1280, 720),
        "1080p" | "fhd" => (1920, 1080),
        "1440p" | "2k" | "qhd" => (2560, 1440),
        "4k" | "uhd" | "2160p" => (3840, 2160),
        "5k" => (5120, 2880),
        "6k" => (6016, 3384),
        "8k" => (7680, 4320),
        other => {
            let (w, h) = other.split_once('x')?;
            (w.parse().ok()?, h.parse().ok()?)
        }
    };
    if !(320..=16384).contains(&w) || !(200..=16384).contains(&h) || hz.is_some_and(|z| !(1..=500).contains(&z)) {
        return None;
    }
    Some((w, h, hz))
}

/// Modes offered by a virtual panel of `w`x`h` pixels: its native size
/// first (what it starts in), then the usual "looks like" sizes of the same
/// aspect ratio down to half size — the list System Settings shows for a
/// real monitor. With HiDPI each of them also gets a 2x-rendered variant.
pub fn modes_for(w: u32, h: u32) -> Vec<(u32, u32)> {
    const WIDTHS: [u32; 22] = [
        1024, 1152, 1280, 1344, 1366, 1440, 1512, 1600, 1680, 1728, 1792, 1920, 2048, 2240, 2304, 2560, 2880, 3008, 3200, 3360, 3840, 4096,
    ];
    let mut out = vec![(w, h)];
    for &cw in WIDTHS.iter().rev() {
        if cw >= w || cw * 2 < w {
            continue;
        }
        let ch = ((cw as u64 * h as u64 + w as u64 / 2) / w as u64) as u32 & !1;
        if !out.contains(&(cw, ch)) {
            out.push((cw, ch));
        }
    }
    let half = (w / 2, h / 2);
    if !out.contains(&half) {
        out.push(half);
    }
    out
}

// ---- state: one file per virtual display, written only by its holder ----

#[derive(Clone, Debug)]
pub struct Virtual {
    pub serial: u32,
    pub pid: u32,
    pub display_id: u32,
    pub spec: Spec,
    pub name: String,
    heartbeat: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn dir() -> PathBuf {
    crate::common::state_path().with_file_name("virtual")
}

fn file_for(serial: u32) -> PathBuf {
    dir().join(format!("{serial}.json"))
}

fn write_virtual(v: &Virtual) {
    let mut m = BTreeMap::new();
    m.insert("serial".to_string(), v.serial.to_string());
    m.insert("pid".to_string(), v.pid.to_string());
    m.insert("display_id".to_string(), v.display_id.to_string());
    m.insert("width".to_string(), v.spec.width.to_string());
    m.insert("height".to_string(), v.spec.height.to_string());
    m.insert("hz".to_string(), v.spec.hz.to_string());
    m.insert("hidpi".to_string(), if v.spec.hidpi { "1" } else { "0" }.to_string());
    m.insert("name".to_string(), v.name.clone());
    m.insert("heartbeat".to_string(), v.heartbeat.to_string());
    write_atomic(&file_for(v.serial), &json_write_object(&m));
}

fn read_virtual(path: &std::path::Path) -> Option<Virtual> {
    let m = json_parse_object(&std::fs::read_to_string(path).ok()?)?;
    let num = |k: &str| m.get(k).and_then(|s| s.parse::<u64>().ok());
    Some(Virtual {
        serial: num("serial")? as u32,
        pid: num("pid")? as u32,
        display_id: num("display_id")? as u32,
        spec: Spec {
            width: num("width")? as u32,
            height: num("height")? as u32,
            hz: num("hz").unwrap_or(60) as u32,
            hidpi: m.get("hidpi").map(|s| s == "1").unwrap_or(true),
        },
        name: m.get("name").cloned().unwrap_or_default(),
        heartbeat: num("heartbeat")?,
    })
}

fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    pid > 0 && unsafe { kill(pid as i32, 0) } == 0
}

/// Live virtual displays; stale files (dead holders) are cleaned up on the way.
pub fn live() -> Vec<Virtual> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(dir()) else { return out };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        match read_virtual(&path) {
            Some(v) if now().saturating_sub(v.heartbeat) <= STALE_AFTER_SECS && pid_alive(v.pid) => out.push(v),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out.sort_by_key(|v| v.serial);
    out
}

// ---- holder ----

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

/// Create the display in this process. It exists as long as the returned
/// object does (`release`) - or until the process ends.
pub unsafe fn create(spec: &Spec, serial: u32, name: &str) -> Result<(Id, u32), String> {
    for c in ["CGVirtualDisplay", "CGVirtualDisplayDescriptor", "CGVirtualDisplaySettings", "CGVirtualDisplayMode"] {
        if class(c).is_null() {
            return Err(format!("{c} not available on this macOS version (needs macOS 11+)"));
        }
    }
    let scale = if spec.hidpi { 2 } else { 1 };
    let desc = send(send(class("CGVirtualDisplayDescriptor"), "alloc"), "init");
    send_id(desc, "setQueue:", dispatch_get_global_queue(0, 0));
    send_id(desc, "setName:", ns_string(name));
    send_u32(desc, "setMaxPixelsWide:", spec.width * scale);
    send_u32(desc, "setMaxPixelsHigh:", spec.height * scale);
    // physical size drives macOS's default scaling choice; ~110 ppi like a desktop monitor
    let mm = |px: u32| px as f64 / 110.0 * 25.4;
    send_size(desc, "setSizeInMillimeters:", CGSize { w: mm(spec.width), h: mm(spec.height) });
    send_u32(desc, "setVendorID:", VENDOR_ID);
    send_u32(desc, "setProductID:", PRODUCT_ID);
    send_u32(desc, "setSerialNum:", serial);

    let display = send_id(send(class("CGVirtualDisplay"), "alloc"), "initWithDescriptor:", desc);
    if display.is_null() {
        return Err("WindowServer refused to create the virtual display".into());
    }
    let modes: Vec<Id> = modes_for(spec.width, spec.height)
        .into_iter()
        .map(|(w, h)| new_mode(w, h, spec.hz as f64))
        .collect();
    let settings = send(send(class("CGVirtualDisplaySettings"), "alloc"), "init");
    send_u32(settings, "setHiDPI:", spec.hidpi as u32);
    send_id(settings, "setModes:", CFArrayCreate(std::ptr::null(), modes.as_ptr(), modes.len() as isize, &kCFTypeArrayCallBacks));
    if !send_bool_id(display, "applySettings:", settings) {
        return Err("applySettings failed (unsupported size/refresh rate?)".into());
    }
    let id = send_ret_u32(display, "displayID");
    if id == 0 {
        return Err("virtual display got no display id".into());
    }
    // wait until it is online *with a mode*, so the caller can configure or
    // capture it right away (a stream started before the first mode lands
    // never delivers a frame of a static screen)
    for _ in 0..50 {
        if online_ids().contains(&id) && has_mode(id) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok((display, id))
}

/// Destroy a display made by `create` in this process.
pub unsafe fn release(display: Id) {
    send(display, "release");
}

fn holder(args: &[String]) -> ! {
    use std::io::Write;
    let get = |i: usize| args.get(i).cloned().unwrap_or_default();
    let spec = Spec {
        width: get(0).parse().unwrap_or(1920),
        height: get(1).parse().unwrap_or(1080),
        hz: get(2).parse().unwrap_or(60),
        hidpi: get(3) == "1",
    };
    let serial: u32 = get(4).parse().unwrap_or(1);
    let name = get(5);
    let mut out = std::io::stdout();
    let (_display, display_id) = match unsafe { create(&spec, serial, &name) } {
        Ok(x) => x,
        Err(e) => {
            let _ = writeln!(out, "err {e}");
            std::process::exit(1);
        }
    };
    let mut v = Virtual { serial, pid: std::process::id(), display_id, spec, name, heartbeat: now() };
    write_virtual(&v);
    let _ = writeln!(out, "ok {display_id}");
    let _ = out.flush();
    // the CLI only reads the handshake line; nothing else may go to stdout
    loop {
        std::thread::sleep(Duration::from_secs(HEARTBEAT_SECS));
        if !file_for(serial).exists() {
            std::process::exit(0); // removed: dropping out of the process ends the display
        }
        v.heartbeat = now();
        write_virtual(&v);
    }
}

// ---- CLI ----

fn kill_holder(v: &Virtual) {
    let _ = std::fs::remove_file(file_for(v.serial));
    if pid_alive(v.pid) {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(v.pid as i32, 15) };
    }
}

fn describe(v: &Virtual) -> String {
    format!(
        "{} \"{}\" ({}x{} @{}Hz{})",
        v.display_id,
        v.name,
        v.spec.width,
        v.spec.height,
        v.spec.hz,
        if v.spec.hidpi { ", HiDPI" } else { "" }
    )
}

/// Spawn a holder for `spec` and wait for its handshake. Returns the new display.
pub fn add(spec: Spec, name: Option<String>) -> Result<Virtual, String> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let used: Vec<u32> = live().iter().map(|v| v.serial).collect();
    let serial = (1..).find(|s| !used.contains(s)).unwrap();
    let name = name.unwrap_or_else(|| format!("monctl Virtual {serial}"));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(["virtual", "--hold"])
        .args([spec.width, spec.height, spec.hz].map(|n| n.to_string()))
        .arg(if spec.hidpi { "1" } else { "0" })
        .arg(serial.to_string())
        .arg(&name)
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
    let line = line.trim();
    if let Some(e) = line.strip_prefix("err ") {
        return Err(e.to_string());
    }
    let Some(id) = line.strip_prefix("ok ").and_then(|s| s.parse::<u32>().ok()) else {
        let _ = child.kill();
        let _ = std::fs::remove_file(file_for(serial));
        return Err("virtual display holder did not come up".into());
    };
    Ok(Virtual { serial, pid: child.id(), display_id: id, spec, name, heartbeat: now() })
}

pub fn remove(v: &Virtual) {
    kill_holder(v);
    // wait for WindowServer to drop it so a follow-up command sees the new layout
    for _ in 0..30 {
        if !online_ids().contains(&v.display_id) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn find(token: &str) -> Option<Virtual> {
    live().into_iter().find(|v| v.display_id.to_string() == token || v.name == token)
}

pub fn run(args: &[String]) -> ! {
    if args.first().map(|s| s.as_str()) == Some("--hold") {
        holder(&args[1..]);
    }
    let mut name = None;
    let mut hidpi = true;
    let mut rest = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => name = Some(it.next().cloned().unwrap_or_else(|| fail("--name needs a value"))),
            "--no-hidpi" | "--lodpi" => hidpi = false,
            "--hidpi" => hidpi = true,
            _ => rest.push(a.as_str()),
        }
    }
    match rest.as_slice() {
        [] | ["list" | "ls"] => {
            let vs = live();
            if vs.is_empty() {
                println!("no virtual displays (create one: monctl virtual add 2560x1440)");
            }
            for v in vs {
                println!("{}", describe(&v));
            }
        }
        ["add" | "new" | "create", size @ ..] => {
            let (w, h, hz) = match size.first() {
                None => (1920, 1080, None),
                Some(s) => parse_size(s).unwrap_or_else(|| fail(&format!("bad size '{s}' - use e.g. 2560x1440, 1920x1080@120, 4k"))),
            };
            match add(Spec { width: w, height: h, hz: hz.unwrap_or(60), hidpi }, name) {
                Ok(v) => println!("added   {}", describe(&v)),
                Err(e) => fail(&format!("cannot create virtual display: {e}")),
            }
        }
        ["rm" | "remove" | "del" | "delete", target] => {
            let targets: Vec<Virtual> = if *target == "all" {
                live()
            } else {
                vec![find(target).unwrap_or_else(|| fail(&format!("no virtual display '{target}' (see `monctl virtual`)")))]
            };
            if targets.is_empty() {
                println!("no virtual displays");
            }
            for v in targets {
                remove(&v);
                println!("removed {}", describe(&v));
            }
        }
        _ => fail("usage: monctl virtual [list] | add [WxH[@Hz]] [--name N] [--no-hidpi] | rm ID|all"),
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2560x1440"), Some((2560, 1440, None)));
        assert_eq!(parse_size("1920x1080@120"), Some((1920, 1080, Some(120))));
        assert_eq!(parse_size("4k"), Some((3840, 2160, None)));
        assert_eq!(parse_size("4K@30Hz"), Some((3840, 2160, Some(30))));
        assert_eq!(parse_size("10x10"), None);
        assert_eq!(parse_size("1920"), None);
    }

    #[test]
    fn modes_start_native_and_keep_aspect() {
        let m = modes_for(3840, 2160);
        assert_eq!(m[0], (3840, 2160));
        assert!(m.contains(&(1920, 1080)));
        assert!(m.contains(&(2560, 1440)));
        assert!(m.iter().all(|&(w, h)| w <= 3840 && w * 2 >= 3840 && (w as f64 / h as f64 - 16.0 / 9.0).abs() < 0.01));
    }
}
