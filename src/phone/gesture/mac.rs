// macOS gesture hook: a CGEventTap over mouse events in the phone display.
//
// Filter: only events whose source process is the remote app's server (the
// tap reads kCGMouseEventSourceUnixProcessID); the physical mouse's events
// come from WindowServer/own-process sources and pass untouched. Replayed
// events carry a private field marker so the tap ignores them.
//
// Without the Accessibility permission CGEventTapCreate returns null - the
// CLI prints where to grant it instead of failing silently. Watch starts
// this with the session, like the Windows hook.

use super::{Gesture, Machine, Pos};
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

type Id = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: Option<unsafe extern "C" fn(u32, u32, Id, Id) -> Id>, info: Id) -> Id;
    fn CGEventGetLocation(e: Id) -> CGPoint;
    fn CGEventTapEnable(tap: Id, on: bool);
    fn CGEventGetIntegerValueField(e: Id, field: u32) -> i64;
    fn CGEventSetIntegerValueField(e: Id, field: u32, value: i64);
    fn CGEventSetLocation(e: Id, location: CGPoint);
    fn CGEventCreateMouseEvent(source: *mut c_void, kind: u32, at: CGPoint, button: u32) -> Id;
    fn CGEventCreateScrollWheelEvent(source: *mut c_void, units: u32, axes: u32, amount: i32) -> Id;
    fn CGEventPost(tap: u32, e: Id);
    fn CFMachPortGetPort(p: Id) -> u32;
    fn CFRunLoopAddSource(run: Id, source: Id, mode: Id);
    fn CFRunLoopGetCurrent() -> Id;
    fn CFRunLoopRun();
    fn CFRelease(p: Id);
    static kCFRunLoopCommonModes: Id;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *mut c_void, port: Id, order: isize) -> Id;
}

const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const TAP_DEFAULT: u32 = 0; // can veto events
const MOUSE_MASK: u64 = MASK_LEFT_DOWN | MASK_LEFT_UP | MASK_DRAGGED | MASK_MOVED;
const MASK_LEFT_DOWN: u64 = 1 << 1;
const MASK_LEFT_UP: u64 = 1 << 2;
const MASK_DRAGGED: u64 = 1 << 6;
const MASK_MOVED: u64 = 1 << 5;
const FIELD_SOURCE_PID: u32 = 39; // kCGMouseEventSourceUnixProcessID
const FIELD_MARKER: u32 = 45; // private/user field nobody else sets
const EVENT_LEFT_DOWN: u32 = 1;
const EVENT_LEFT_UP: u32 = 2;
const EVENT_MOVED: u32 = 5;
const EVENT_DRAGGED: u32 = 6;
const SCROLL_LINE_UNITS: u32 = 0;
const POST_SESSION: u32 = 0;

static MACHINE: Mutex<Option<Machine>> = Mutex::new(None);
static AREA: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
static REPLAYS: AtomicI32 = AtomicI32::new(0);
static REMOTE_PID: AtomicI32 = AtomicI32::new(0);
static SCALE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

unsafe extern "C" fn tap_cb(_proxy: u32, kind: u32, event: Id, _info: Id) -> Id {
    // Our own replays: marked, passed on untouched.
    if CGEventGetIntegerValueField(event, FIELD_MARKER) != 0 {
        return event;
    }
    let source = CGEventGetIntegerValueField(event, FIELD_SOURCE_PID) as i32;
    let remote = REMOTE_PID.load(Ordering::SeqCst);
    if remote == 0 || source != remote {
        return event; // physical input or unknown source: untouched
    }
    // The mouse event's location: fields 33/34 are kCGMouseEventX/Y? No -
    // use CGEventGetLocation, a plain C call, to stay exact.
    let at = CGEventGetLocation(event);
    let area = *AREA.lock().unwrap();
    let inside = area.is_some_and(|a| at.x >= a.0 && at.x <= a.0 + a.2 && at.y >= a.1 && at.y <= a.1 + a.3);
    if !inside {
        return event;
    }
    let gesture = {
        let mut guard = MACHINE.lock().unwrap();
        let Some(machine) = guard.as_mut() else { return event };
        let t = now();
        match kind {
            EVENT_LEFT_DOWN => machine.on_down(Pos { x: at.x, y: at.y }, t),
            EVENT_LEFT_UP => machine.on_up(Pos { x: at.x, y: at.y }, t),
            EVENT_DRAGGED | EVENT_MOVED => machine.on_move(Pos { x: at.x, y: at.y }, t),
            _ => Gesture::PassThrough,
        }
    };
    if std::env::var_os("MONCTL_GESTURE_DEBUG").is_some() {
        eprintln!("[gesture] kind={kind} at=({:.0},{:.0}) -> {gesture:?}", at.x, at.y);
    }
    match gesture {
        Gesture::PassThrough => event,
        Gesture::None | Gesture::StopOnly => std::ptr::null_mut(), // veto
        Gesture::Scroll { dy, .. } => {
            veto_scroll();
            // Content scrolls opposite to finger drag.
            let amount = -(dy as i32);
            let e = CGEventCreateScrollWheelEvent(std::ptr::null_mut(), SCROLL_LINE_UNITS, 1, amount.clamp(-50, 50));
            REPLAYS.fetch_add(1, Ordering::SeqCst);
            CGEventSetIntegerValueField(e, FIELD_MARKER, 1);
            CGEventPost(POST_SESSION, e);
            CFRelease(e);
            std::ptr::null_mut()
        }
        Gesture::SynthDown => {
            replay_mouse(EVENT_LEFT_DOWN, at);
            std::ptr::null_mut()
        }
        Gesture::RightClick => {
            replay_mouse(EVENT_LEFT_UP, at);
            // right down + up
            let down = CGEventCreateMouseEvent(std::ptr::null_mut(), 3 /* rightDown */, at, 1);
            CGEventSetIntegerValueField(down, FIELD_MARKER, 1);
            CGEventPost(POST_SESSION, down);
            let up = CGEventCreateMouseEvent(std::ptr::null_mut(), 4 /* rightUp */, at, 1);
            CGEventSetIntegerValueField(up, FIELD_MARKER, 1);
            CGEventPost(POST_SESSION, up);
            std::ptr::null_mut()
        }
        Gesture::Inertia { vy, .. } => {
            // No native inertia on macOS through wheel posts; a decaying
            // timer like Windows keeps the feel consistent.
            start_inertia(vy);
            std::ptr::null_mut()
        }
        Gesture::Close => {
            std::ptr::null_mut()
        }
    }
}

fn veto_scroll() {
    INERTIA_CANCEL.fetch_add(1, Ordering::SeqCst);
}

static INERTIA_CANCEL: AtomicI32 = AtomicI32::new(0);

fn replay_mouse(kind: u32, at: CGPoint) {
    unsafe {
        let e = CGEventCreateMouseEvent(std::ptr::null_mut(), kind, at, 0);
        CGEventSetIntegerValueField(e, FIELD_MARKER, 1);
        CGEventPost(POST_SESSION, e);
        CFRelease(e);
    }
}

fn scroll(lines: i32) {
    unsafe {
        let e = CGEventCreateScrollWheelEvent(std::ptr::null_mut(), SCROLL_LINE_UNITS, 1, lines);
        CGEventSetIntegerValueField(e, FIELD_MARKER, 1);
        CGEventPost(POST_SESSION, e);
        CFRelease(e);
    }
}

fn start_inertia(mut vy: f64) {
    let generation = INERTIA_CANCEL.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        loop {
            if INERTIA_CANCEL.load(Ordering::SeqCst) != generation || vy.abs() < 3.0 {
                return;
            }
            scroll(-(vy as i32).clamp(-50, 50));
            vy *= 0.92;
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
}

/// The remote app's server pid, resolved once per session (pgrep).
fn resolve_remote_pid() -> i32 {
    let out = std::process::Command::new("/usr/bin/pgrep").arg("-x").arg("UURemoteServer").output();
    if let Ok(o) = out {
        if let Ok(text) = String::from_utf8(o.stdout) {
            if let Some(first) = text.lines().next() {
                if let Ok(pid) = first.trim().parse::<i32>() {
                    return pid;
                }
            }
        }
    }
    0
}

pub fn run(area: (f64, f64, f64, f64), scale: f64) {
    *AREA.lock().unwrap() = Some(area);
    *MACHINE.lock().unwrap() = Some(Machine::new(scale));
    let scale_bits = (scale as f64).to_bits();
    SCALE.store(scale_bits, Ordering::SeqCst);
    REMOTE_PID.store(resolve_remote_pid(), Ordering::SeqCst);
    if REMOTE_PID.load(Ordering::SeqCst) == 0 {
        eprintln!("monctl: the remote app's server is not running - gestures watch for UURemoteServer and will idle until it appears");
    }
    unsafe {
        let mask = MASK_LEFT_DOWN | MASK_LEFT_UP | MASK_DRAGGED | MASK_MOVED;
        let tap = CGEventTapCreate(SESSION_TAP, HEAD_INSERT, TAP_DEFAULT, mask, Some(tap_cb), std::ptr::null_mut());
        if tap.is_null() {
            eprintln!("monctl: cannot create the event tap - grant this binary (or sshd for SSH runs) the Accessibility permission: System Settings > Privacy & Security > Accessibility");
            return;
        }
        CGEventTapEnable(tap, true);
        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
        CFRunLoopRun(); // runs until the process ends (watch kills it)
    }
}
