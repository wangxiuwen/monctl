// Windows gesture hook: a low-level mouse hook (WH_MOUSE_LL) that
// re-interprets the remote app's injected events inside the phone display.
//
// What counts as "the remote app's events": anything carrying the
// LLMHF_INJECTED flag - physical mice do not set it. Our own replayed
// events are injected too, so every SendInput bumps a replay counter the
// hook consumes before interpreting anything else.
//
// The hook runs on its own thread with a message loop; translations are
// replayed inline (scroll = wheel messages, drag-select = a fresh button
// press) and inertia runs as a decay animation thread that any tap stops.

use super::{Gesture, Machine, Pos};
use crate::phone::window::win as mover;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

type HHOOK = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;

const WH_MOUSE_LL: i32 = 14;
const LLMHF_INJECTED: u32 = 1;
const WM_MOUSEMOVE: u32 = 0x200;
const WM_LBUTTONDOWN: u32 = 0x201;
const WM_LBUTTONUP: u32 = 0x202;
const WM_RBUTTONDOWN: u32 = 0x204;
const WM_RBUTTONUP: u32 = 0x205;
const MOUSEEVENTF_MOVE: u32 = 0x1;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x2;
const MOUSEEVENTF_LEFTUP: u32 = 0x4;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x8;
const MOUSEEVENTF_RIGHTUP: u32 = 0x10;
const MOUSEEVENTF_WHEEL: u32 = 0x800;
const WHEEL_DELTA: i32 = 120;

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

impl Default for Point {
    fn default() -> Self {
        Point { x: 0, y: 0 }
    }
}

#[repr(C)]
#[derive(Default)]
struct MSG {
    hwnd: *mut c_void,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    pt: Point,
    time: u32,
    extra: [u64; 3],
}

#[repr(C)]
struct MsgHookInfo {
    pt: Point,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra: usize,
    w_param: WPARAM,
}

#[repr(C)]
struct Input {
    kind: u32,
    _pad: u32,
    u: InputUnion,
}

#[repr(C)]
#[derive(Clone, Copy)]
union InputUnion {
    mouse: MouseInput,
    pad: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseInput {
    dx: i32,
    dy: i32,
    mouse_data: i32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[link(name = "user32")]
extern "system" {
    fn SetWindowsHookExW(id: i32, proc: Option<unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT>, inst: *mut c_void, thread: u32) -> HHOOK;
    fn UnhookWindowsHookEx(h: HHOOK) -> i32;
    fn CallNextHookEx(h: HHOOK, code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    fn GetMessageW(msg: *mut MSG, h: *mut c_void, min: u32, max: u32) -> i32;
    fn SendInput(n: u32, inputs: *const Input, size: i32) -> u32;
    fn PostThreadMessageW(thread: u32, msg: u32, w: WPARAM, l: LPARAM) -> i32;
    fn PostMessageW(h: *mut c_void, msg: u32, w: WPARAM, l: LPARAM) -> i32;
    fn GetCurrentThreadId() -> u32;
}

const WM_QUIT: u32 = 0x12;

/// Raw handles are not Share/Send; the hook handle is only ever touched by
/// the hook thread itself, so a transparent wrapper is honest here.
#[repr(transparent)]
struct ThreadPtr(*mut c_void);
unsafe impl Send for ThreadPtr {}
unsafe impl Sync for ThreadPtr {}
static HOOK: OnceLock<ThreadPtr> = OnceLock::new();
/// SendInput we ourselves issued, awaiting pass-through in the hook.
static REPLAYS: AtomicI32 = AtomicI32::new(0);
/// The machine is fed from the hook thread only; the inertia thread only
/// touches its own state.
static MACHINE: Mutex<Option<Machine>> = Mutex::new(None);
static INERTIA_CANCEL: AtomicU64 = AtomicU64::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// The placed window's close button (global px), set from the CLI thread.
static CLOSE_BUTTON: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
/// Coordinates for the window mover, used by the close interception.
static MOVER_ID: Mutex<Option<String>> = Mutex::new(None);
/// The hook only acts inside this rect (the phone display, global px).
static AREA: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn send_mouse(flags: u32, dx: i32, dy: i32, data: i32) {
    REPLAYS.fetch_add(1, Ordering::SeqCst);
    let input = Input {
        kind: 0, // INPUT_MOUSE
        _pad: 0,
        u: InputUnion { mouse: MouseInput { dx, dy, mouse_data: data, flags, time: 0, extra: 0 } },
    };
    unsafe {
        SendInput(1, &input, std::mem::size_of::<Input>() as i32);
    }
}

fn wheel(dy_points: f64) {
    // Points to wheel detents: a detent scrolls ~120px of content, and the
    // display is 2x, so a point of drag is half a pixel of content.
    let px = dy_points * 2.0;
    let detents = px / 120.0 * 2.0; // ~2x detent sensitivity feels right
    let mut left = (detents * WHEEL_DELTA as f64).round() as i32;
    while left != 0 {
        let step = left.clamp(-WHEEL_DELTA, WHEEL_DELTA);
        send_mouse(MOUSEEVENTF_WHEEL, 0, 0, step);
        left -= step;
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        if std::env::var_os("MONCTL_GESTURE_DEBUG").is_some() {
            let info = &*(lparam as *const MsgHookInfo);
            eprintln!("[hook] wp={wparam:#x} pt=({},{}), flags={:#x}", info.pt.x, info.pt.y, info.flags);
        }
        let info = &*(lparam as *const MsgHookInfo);
        let injected = info.flags & LLMHF_INJECTED != 0;
        if injected {
            // Pass our own replays straight through.
            if REPLAYS.load(Ordering::SeqCst) > 0 {
                REPLAYS.fetch_sub(1, Ordering::SeqCst);
                return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
            }
            let area = *AREA.lock().unwrap();
            let inside = area.is_some_and(|a| {
                (info.pt.x as f64) >= a.0 && (info.pt.x as f64) <= a.0 + a.2 && (info.pt.y as f64) >= a.1 && (info.pt.y as f64) <= a.1 + a.3
            });
            if inside {
                return interpret(wparam, info.pt.x as f64, info.pt.y as f64);
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

/// Feed the machine; the return value is the hook's answer for this event
/// (pass it on, or swallow it because a replay replaces it).
fn interpret(wparam: WPARAM, x: f64, y: f64) -> LRESULT {
    let pos = Pos { x, y };
    let mut guard = MACHINE.lock().unwrap();
    let Some(machine) = guard.as_mut() else { return 1 };
    let gesture = match wparam as u32 {
        WM_MOUSEMOVE => machine.on_move(pos, now()),
        WM_LBUTTONDOWN => {
            INERTIA_CANCEL.fetch_add(1, Ordering::SeqCst); // a press stops the fly
            machine.on_down(pos, now())
        }
        WM_LBUTTONUP => machine.on_up(pos, now()),
        _ => Gesture::PassThrough,
    };
    if std::env::var_os("MONCTL_GESTURE_DEBUG").is_some() {
        eprintln!("[gesture] {gesture:?} at ({x:.0},{y:.0})");
    }
    match gesture {
        Gesture::PassThrough => 1, // let the original through
        Gesture::None | Gesture::StopOnly => 0, // swallow
        Gesture::Scroll { dy, .. } => {
            wheel(dy);
            0
        }
        Gesture::SynthDown => {
            send_mouse(MOUSEEVENTF_LEFTDOWN, 0, 0, 0);
            0
        }
        Gesture::RightClick => {
            send_mouse(MOUSEEVENTF_RIGHTDOWN, 0, 0, 0);
            send_mouse(MOUSEEVENTF_RIGHTUP, 0, 0, 0);
            0
        }
        Gesture::Inertia { vx, vy } => {
            start_inertia(vx, vy);
            0
        }
        Gesture::Close => {
            // Restore the window to its recorded place, then close it, so
            // the app never saves the phone-display size as its own.
            let id = MOVER_ID.lock().unwrap().clone();
            if let Some(id) = id {
                crate::phone::window::restore_all_placed();
                if let Some(h) = mover::hwnd_of(&id) {
                    unsafe { PostMessageW(h, 0x10 /* WM_CLOSE */, 0, 0) };
                }
            }
            0
        }
    }
}

/// Inertia: wheel messages at decaying speed until a tap stops them.
fn start_inertia(mut vx: f64, mut vy: f64) {
    let generation = INERTIA_CANCEL.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        loop {
            if INERTIA_CANCEL.load(Ordering::SeqCst) != generation {
                return; // a newer gesture took over
            }
            if vx.abs() < 5.0 && vy.abs() < 5.0 {
                return;
            }
            wheel(vy);
            vx *= 0.92;
            vy *= 0.92;
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
}

/// Install the hook and run until the process exits or `stop` is called.
/// `area` is the phone display in global px; `scale` is px per point (2).
pub fn run(area: (f64, f64, f64, f64), scale: f64) {
    *AREA.lock().unwrap() = Some(area);
    *MACHINE.lock().unwrap() = Some(Machine::new(scale));
    dpi_and_mover();
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), std::ptr::null_mut(), 0) };
    if hook.is_null() {
        eprintln!("monctl: cannot install the mouse hook");
        return;
    }
    let _ = HOOK.set(ThreadPtr(hook));
    HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    // A low-level hook needs a message loop on this thread.
    unsafe {
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
        }
        UnhookWindowsHookEx(hook);
    }
}

/// Tell the hook which window's close button to intercept.
pub fn set_close_target(id: &str, close_button: Option<(f64, f64, f64, f64)>) {
    *MOVER_ID.lock().unwrap() = Some(id.to_string());
    *CLOSE_BUTTON.lock().unwrap() = close_button;
    if let Some(m) = MACHINE.lock().unwrap().as_mut() {
        m.set_close_button(close_button);
    }
}

/// A tap anywhere stops the inertia fly (the machine emits StopOnly; the
/// animation thread also dies here).
pub fn stop_inertia() {
    INERTIA_CANCEL.fetch_add(1, Ordering::SeqCst);
}

fn dpi_and_mover() {
    // The mover sets per-monitor DPI awareness at first use; warm it here so
    // the hook thread's coordinates agree with the CLI thread's.
    let _ = mover::list_windows();
}


/// Stop the hook from another thread (the watch loop does this on session end).
pub fn stop() {
    let t = HOOK_THREAD.load(Ordering::SeqCst);
    if t != 0 {
        unsafe { PostThreadMessageW(t, WM_QUIT, 0, 0) };
        HOOK_THREAD.store(0, Ordering::SeqCst);
    }
}
