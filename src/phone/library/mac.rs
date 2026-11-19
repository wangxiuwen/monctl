// macOS: the app library's data (NSWorkspace + the Applications folders)
// and its window (an NSWindow + WKWebView on the phone display, loading the
// localhost page the library server runs).
//
// objc struct *returns* are avoided (see window/mac.rs); struct *arguments*
// are fine - virt.rs already passes CGSize through msgSend. Launch and quit
// go through `open -a` and `osascript`: no NSWorkspace callbacks needed.

use super::{App, Catalog};
use crate::phone::window::mac as mover;
use crate::virt::{class, msg_fn, ns_string, sel, send, send_id};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

type Id = *mut c_void;
type Sel = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Raw objc pointers are not Send/Sync; these handles live in one place and
/// are only dereferenced on the thread that shows the window.
#[repr(transparent)]
struct Obj(Id);
unsafe impl Send for Obj {}
unsafe impl Sync for Obj {}

static WINDOW: Mutex<Option<Obj>> = Mutex::new(None);
static WEB: OnceLock<Obj> = OnceLock::new();

pub struct MacCatalog;

impl Catalog for MacCatalog {
    fn apps(&self) -> Vec<App> {
        let mut out: Vec<App> = Vec::new();
        let running = running_apps();
        for (name, pid) in &running {
            out.push(App { name: name.clone(), pid: *pid, running: true });
        }
        for name in installed_apps() {
            if running.iter().any(|(n, _)| n == &name) {
                continue;
            }
            out.push(App { name, pid: 0, running: false });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn open(&self, name: &str) -> Result<(), String> {
        let status = std::process::Command::new("/usr/bin/open")
            .arg("-a")
            .arg(name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("open -a {name} failed"))
        }
    }

    fn quit(&self, name: &str) -> Result<(), String> {
        let script = format!("tell application \"{}\" to quit", name.replace('"', ""));
        let status = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("quit {name} failed (it may not be scriptable)"))
        }
    }
}

unsafe fn send_index(obj: Id, s: &str, i: isize) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, isize) -> Id = std::mem::transmute(msg_fn());
    f(obj, sel(s), i)
}

unsafe fn send_ret_i32(obj: Id, s: &str) -> i32 {
    let f: unsafe extern "C" fn(Id, Sel) -> i32 = std::mem::transmute(msg_fn());
    f(obj, sel(s))
}

unsafe fn send_rect(obj: Id, s: &str, r: NSRect) {
    let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(msg_fn());
    f(obj, sel(s), r)
}

unsafe fn send_rect_id(obj: Id, s: &str, r: NSRect, a: Id) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, NSRect, Id) -> Id = std::mem::transmute(msg_fn());
    f(obj, sel(s), r, a)
}

fn ns_string_of(v: Id) -> String {
    unsafe {
        let utf8: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char = std::mem::transmute(msg_fn());
        let p = utf8(v, sel("UTF8String"));
        if p.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn running_apps() -> Vec<(String, u32)> {
    unsafe {
        let ws = send(class("NSWorkspace"), "sharedWorkspace");
        let apps = send(ws, "runningApplications");
        let count: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(msg_fn());
        let n = count(apps, sel("count"));
        let mut out = Vec::new();
        for i in 0..n {
            let app = send_index(apps, "objectAtIndex:", i);
            if send_ret_i32(app, "activationPolicy") != 0 {
                continue; // regular apps only
            }
            let pid = send_ret_i32(app, "processIdentifier");
            if pid <= 0 {
                continue;
            }
            out.push((ns_string_of(send(app, "localizedName")), pid as u32));
        }
        out
    }
}

/// App names from /Applications and /System/Applications (one level).
fn installed_apps() -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["/Applications", "/System/Applications"] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".app") {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Put the library window on the phone display, filling its usable area.
pub fn show() -> Result<(), String> {
    let area = mover::phone_display_area().ok_or("phone display is not on")?;
    let fit = mover::held_fit().ok_or("phone display is not on")?;
    let frame = crate::phone::window::target_frame(&area, fit.base_width);
    unsafe {
        let existing = WINDOW.lock().unwrap().as_ref().map(|o| o.0);
        let window = match existing {
            Some(w) => w,
            None => {
                let w = make_window();
                *WINDOW.lock().unwrap() = Some(Obj(w));
                w
            }
        };
        // NSWindow frames use a bottom-left origin; the area is top-left.
        let y = area.bounds.1 + area.bounds.3 - frame.1 - frame.3;
        send_rect(window, "setFrame:display:", NSRect { x: frame.0, y, w: frame.2, h: frame.3 });
        send(window, "makeKeyAndOrderFront:");
        let web = WEB.get_or_init(|| Obj(make_web_view())).0;
        load_page(web);
        // The content view was built before the frame was known; size it now.
        send_rect(web, "setFrame:", NSRect { x: 0.0, y: 0.0, w: frame.2, h: frame.3 });
    }
    start_server();
    Ok(())
}

pub fn hide() {
    unsafe {
        if let Some(obj) = WINDOW.lock().unwrap().take() {
            send(obj.0, "orderOut:");
        }
    }
}

/// The library window follows the display's step changes.
pub fn refit() -> Result<(), String> {
    show()
}

unsafe fn make_window() -> Id {
    let window = send(class("NSWindow"), "alloc");
    // init gives a zero frame; setFrame below places it. 15 = titled |
    // closable | miniaturizable | resizable.
    let window = send(window, "init");
    send_id(window, "setTitle:", ns_string("monctl 手机屏"));
    let set_u32: unsafe extern "C" fn(Id, Sel, u64) = std::mem::transmute(msg_fn());
    set_u32(window, sel("setStyleMask:"), 15);
    let set_bool: unsafe extern "C" fn(Id, Sel, bool) = std::mem::transmute(msg_fn());
    set_bool(window, sel("setReleasedWhenClosed:"), false);
    send(window, "setMovable:");
    window
}

unsafe fn make_web_view() -> Id {
    let config = send(send(class("WKWebViewConfiguration"), "alloc"), "init");
    let zero = NSRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    let web = send_rect_id(send(class("WKWebView"), "alloc"), "initWithFrame:configuration:", zero, config);
    let url = send_id(class("NSURL"), "URLWithString:", ns_string(&format!("http://127.0.0.1:{}/", super::PORT)));
    let req = send_id(class("NSURLRequest"), "requestWithURL:", url);
    send_id(web, "loadRequest:", req);
    web
}

unsafe fn load_page(web: Id) {
    let Some(window) = WINDOW.lock().unwrap().as_ref().map(|o| o.0) else { return };
    send_id(window, "setContentView:", web);
}

/// The HTTP server behind the page, started once per process.
fn start_server() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| {
            let _ = super::run_server(MacCatalog);
        });
    });
}
