// macOS window mover: Accessibility (AX) for windows, NSScreen for the
// display's usable area.
//
// AX is plain C on top of HID event system calls - no objc classes needed
// for windows themselves; the app enumeration and screen lookup go through
// NSWorkspace/NSScreen via the objc runtime (the same zero-dep bridge
// virt.rs uses). Struct-returning objc calls are avoided by asking
// NSStringFromRect for the frames as text ("{{x, y}, {w, h}}"), which keeps
// one code path for Apple Silicon and Intel.
//
// AX calls into other processes need the Accessibility permission; without
// it the queries come back empty and the CLI says so.

use super::{DisplayArea, Movable, WindowId};
use crate::virt::{class, msg_fn, ns_string, sel, send, send_bool_id, send_id, send_ret_u32, send_size, send_u32, CGSize};
use std::ffi::{c_char, c_void, CStr};

type Id = *mut c_void;
type Sel = *mut c_void;

// ---- objc helpers (struct-free) -------------------------------------------

pub(crate) unsafe fn send_index(obj: Id, s: &str, i: isize) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, isize) -> Id = std::mem::transmute(msg_fn());
    f(obj, sel(s), i)
}

unsafe fn send_ret_i32(obj: Id, s: &str) -> i32 {
    let f: unsafe extern "C" fn(Id, Sel) -> i32 = std::mem::transmute(msg_fn());
    f(obj, sel(s))
}

unsafe fn send_ret_ptr(obj: Id, s: &str) -> *const c_char {
    let f: unsafe extern "C" fn(Id, Sel) -> *const c_char = std::mem::transmute(msg_fn());
    f(obj, sel(s))
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// objc struct return: arm64 moves it in registers through plain msgSend,
/// x86_64 needs objc_msgSend_stret (NSRect is 32 bytes).
unsafe fn send_rect(obj: Id, s: &str) -> NSRect {
    #[cfg(target_arch = "x86_64")]
    unsafe extern "C" {
        fn objc_msgSend_stret();
    }
    #[cfg(target_arch = "x86_64")]
    {
        let f: unsafe extern "C" fn(*mut NSRect, Id, Sel) = std::mem::transmute(objc_msgSend_stret as *const c_void);
        let mut r = NSRect::default();
        f(&mut r, obj, sel(s));
        r
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect = std::mem::transmute(msg_fn());
        f(obj, sel(s))
    }
}

fn ns_string_of(v: Id) -> String {
    unsafe {
        let p = send_ret_ptr(v, "UTF8String");
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

// ---- Accessibility ----------------------------------------------------------

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> Id;
    fn AXUIElementCreateApplication(pid: i32) -> Id;
    fn AXUIElementCopyAttributeValue(e: Id, attr: Id, out: *mut Id) -> i32;
    fn AXUIElementSetAttributeValue(e: Id, attr: Id, val: Id) -> i32;
    fn AXUIElementSetMessagingTimeout(e: Id, t: f64);
    fn AXValueCreate(t: u32, v: *const c_void) -> Id;
    fn AXValueGetValue(v: Id, t: u32, out: *mut c_void) -> bool;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

/// The AX attribute names are stable public strings; building CFStrings from
/// them avoids linking the kAX* constant symbols, which the ApplicationServices
/// umbrella does not re-export as data on current SDKs.
fn attr_position() -> Id {
    unsafe { ns_string("AXPosition") }
}
fn attr_size() -> Id {
    unsafe { ns_string("AXSize") }
}
fn attr_title() -> Id {
    unsafe { ns_string("AXTitle") }
}
fn attr_windows() -> Id {
    unsafe { ns_string("AXWindows") }
}
fn attr_focused_app() -> Id {
    unsafe { ns_string("AXFocusedApplication") }
}
fn attr_focused_window() -> Id {
    unsafe { ns_string("AXFocusedWindow") }
}
fn attr_minimized() -> Id {
    unsafe { ns_string("AXMinimized") }
}
fn attr_fullscreen() -> Id {
    unsafe { ns_string("AXFullScreen") }
}
fn attr_close_button() -> Id {
    unsafe { ns_string("AXCloseButton") }
}
fn attr_pid() -> Id {
    unsafe { ns_string("AXPID") }
}

const AX_VALUE_POINT: u32 = 1;
const AX_VALUE_SIZE: u32 = 2;

/// kAXErrorNotificationUnsupported / API restrictions surface as nonzero
/// codes; empty results without an error mean the permission is missing.
fn ax_copy(e: Id, attr: Id) -> Option<Id> {
    unsafe {
        let mut v: Id = std::ptr::null_mut();
        if AXUIElementCopyAttributeValue(e, attr, &mut v) == 0 && !v.is_null() {
            Some(v)
        } else {
            None
        }
    }
}

fn ax_point(e: Id, attr: Id) -> Option<CGPoint> {
    ax_copy_value(e, attr, AX_VALUE_POINT).map(|v| CGPoint { x: v.0, y: v.1 })
}

fn ax_size(e: Id, attr: Id) -> Option<(f64, f64)> {
    ax_copy_value(e, attr, AX_VALUE_SIZE)
}

/// Read an AXValue attribute as two f64s (works for point and size).
fn ax_copy_value(e: Id, attr: Id, kind: u32) -> Option<(f64, f64)> {
    let v = ax_copy(e, attr)?;
    unsafe {
        let mut out = (0f64, 0f64);
        if AXValueGetValue(v, kind, &mut out as *mut _ as *mut c_void) {
            Some(out)
        } else {
            None
        }
    }
}

fn ax_bool(e: Id, attr: Id) -> Option<bool> {
    let v = ax_copy(e, attr)?;
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> bool = std::mem::transmute(crate::virt::msg_fn());
        Some(f(v, sel("boolValue")))
    }
}

fn ax_set_bool(e: Id, attr: Id, value: bool) {
    unsafe {
        // NSNumber bridges to CFBoolean, which the AX attributes expect.
        let f: unsafe extern "C" fn(Id, Sel, isize) -> Id = std::mem::transmute(msg_fn());
        let num = f(class("NSNumber"), sel("numberWithInteger:"), if value { 1 } else { 0 });
        AXUIElementSetAttributeValue(e, attr, num);
    }
}

fn ax_set_point(e: Id, attr: Id, p: CGPoint) -> bool {
    unsafe {
        let v = AXValueCreate(AX_VALUE_POINT, &p as *const _ as *const c_void);
        AXUIElementSetAttributeValue(e, attr, v) == 0
    }
}

fn ax_set_size(e: Id, attr: Id, s: CGSize) -> bool {
    unsafe {
        let v = AXValueCreate(AX_VALUE_SIZE, &s as *const _ as *const c_void);
        AXUIElementSetAttributeValue(e, attr, v) == 0
    }
}

// ---- apps and windows --------------------------------------------------------

/// Regular, on-screen apps (the ones with windows a user can move).
fn regular_apps() -> Vec<(Id, i32)> {
    unsafe {
        let ws = send(class("NSWorkspace"), "sharedWorkspace");
        let apps = send(ws, "runningApplications");
        let n = send_ret_i32(apps, "count");
        let mut out = Vec::new();
        for i in 0..n {
            let app = send_index(apps, "objectAtIndex:", i as isize);
            if send_ret_i32(app, "activationPolicy") != 0 {
                continue; // NSApplicationActivationPolicyRegular
            }
            let pid = send_ret_i32(app, "processIdentifier");
            if pid <= 0 {
                continue;
            }
            out.push((app, pid));
        }
        out
    }
}

fn ax_app(pid: i32) -> Id {
    unsafe { AXUIElementCreateApplication(pid) }
}

/// All AX windows of one app, with their titles.
fn app_windows(pid: i32) -> Vec<Id> {
    let e = ax_app(pid);
    unsafe { AXUIElementSetMessagingTimeout(e, 1.0) };
    match ax_copy(e, attr_windows()) {
        Some(arr) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> i32 = std::mem::transmute(msg_fn());
            let n = f(arr, sel("count"));
            (0..n).map(|i| send_index(arr, "objectAtIndex:", i as isize)).collect()
        },
        None => vec![],
    }
}

fn window_title(w: Id) -> String {
    ax_copy(w, attr_title()).map(ns_string_of).unwrap_or_default()
}

fn window_frame(w: Id) -> (f64, f64, f64, f64) {
    let p = ax_point(w, attr_position()).unwrap_or(CGPoint::default());
    let s = ax_size(w, attr_size()).unwrap_or((0.0, 0.0));
    (p.x, p.y, s.0, s.1)
}

// ---- mover API ----------------------------------------------------------------

/// Whether this process may drive other apps' windows at all (TCC).
pub fn trusted() -> bool {
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

/// Ask for the Accessibility permission with the standard system dialog
/// ("monctl wants to control this computer"). Works over SSH: the dialog
/// appears on the Mac's own screen; the entry lands in the Accessibility
/// list automatically, highlighted. Returns whether we are trusted now.
pub fn trust_prompt() -> bool {
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: Id) -> bool;
        fn CFDictionaryCreate(alloc: *const c_void, keys: *const c_void, values: *const c_void, n: isize, key_cb: *const c_void, value_cb: *const c_void) -> Id;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        static kCFBooleanTrue: Id;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> Id;
    }
    const UTF8: u32 = 0x0800_0100;
    unsafe {
        let key = CFStringCreateWithCString(std::ptr::null(), c"AXTrustedCheckOptionPrompt".as_ptr(), UTF8);
        let keys: [Id; 1] = [key];
        let values: [Id; 1] = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr() as *const c_void,
            values.as_ptr() as *const c_void,
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        AXIsProcessTrustedWithOptions(opts)
    }
}

pub fn list_windows() -> Vec<Movable> {
    if !trusted() {
        eprintln!(
            "monctl: this process lacks the Accessibility permission (System Settings > Privacy & Security > Accessibility); for SSH runs, grant it to /usr/sbin/sshd - until then no windows are visible"
        );
        return Vec::new();
    }
    let mut out = Vec::new();
    for (_app, pid) in regular_apps() {
        for w in app_windows(pid) {
            let title = window_title(w);
            if title.is_empty() {
                continue;
            }
            let frame = window_frame(w);
            if frame.2 < 40.0 || frame.3 < 40.0 {
                continue;
            }
            let id = format!("{pid}:{}", title);
            out.push(Movable {
                id,
                pid: pid as u32,
                title: title.clone(),
                app: app_name(pid),
                frame,
                minimized: ax_bool(w, attr_minimized()).unwrap_or(false),
                fullscreen: ax_bool(w, attr_fullscreen()).unwrap_or(false),
            });
        }
    }
    out
}

fn app_name(pid: i32) -> String {
    regular_apps()
        .into_iter()
        .find(|(_, p)| *p == pid)
        .and_then(|(app, _)| ax_copy(app, attr_title()).map(ns_string_of))
        .unwrap_or_default()
}

pub fn focused_window() -> Option<Movable> {
    unsafe {
        let wide = AXUIElementCreateSystemWide();
        let app = ax_copy(wide, attr_focused_app())?;
        let pid = ax_copy(app, attr_pid()).map(|v| ns_string_of(send(v, "stringValue"))).and_then(|s| s.trim().parse::<i32>().ok())?;
        let w = ax_copy(app, attr_focused_window())?;
        let title = window_title(w);
        let frame = window_frame(w);
        let id = format!("{pid}:{title}");
        Some(Movable {
            id,
            pid: pid as u32,
            title,
            app: app_name(pid),
            frame,
            minimized: ax_bool(w, attr_minimized()).unwrap_or(false),
            fullscreen: ax_bool(w, attr_fullscreen()).unwrap_or(false),
        })
    }
}

/// Resolve a "pid:title" id back to an AX window. Titles drift while a
/// window sits on the phone display, so a window already inside the phone
/// display's bounds wins as a fallback.
fn resolve(id: &WindowId) -> Option<(Id, u32)> {
    let (pid, _) = id.split_once(':')?;
    let pid = pid.parse::<u32>().ok()?;
    let stored_title = id.split_once(':')?.1.to_string();
    let phone = phone_bounds();
    for w in app_windows(pid as i32) {
        let title = window_title(w);
        if title == stored_title {
            return Some((w, pid));
        }
    }
    if let Some(b) = phone {
        for w in app_windows(pid as i32) {
            let f = window_frame(w);
            if f.0 >= b.0 - 2.0 && f.1 >= b.1 - 2.0 && f.0 + f.2 <= b.0 + b.2 + 2.0 && f.1 + f.3 <= b.1 + b.3 + 2.0 {
                return Some((w, pid));
            }
        }
    }
    None
}

fn phone_bounds() -> Option<(f64, f64, f64, f64)> {
    let id = crate::phone::display::mac::held_display_id()?;
    Some(unsafe { CGDisplayBounds(id) }).map(|b| (b.x, b.y, b.w, b.h))
}

pub fn refresh(id: &WindowId) -> Option<Movable> {
    let (w, pid) = resolve(id)?;
    let title = window_title(w);
    if title.is_empty() {
        return None;
    }
    Some(Movable {
        id: id.clone(),
        pid,
        title,
        app: app_name(pid as i32),
        frame: window_frame(w),
        minimized: ax_bool(w, attr_minimized()).unwrap_or(false),
        fullscreen: ax_bool(w, attr_fullscreen()).unwrap_or(false),
    })
}

pub fn set_frame(id: &WindowId, frame: (f64, f64, f64, f64)) {
    let Some((w, _)) = resolve(id) else { return };
    // Size, position, size - twice: some apps re-clamp once after a move,
    // and the second write lands without fighting the first (Snug's dance).
    for _ in 0..2 {
        unsafe {
            ax_set_size(w, attr_size(), CGSize { w: frame.2, h: frame.3 });
            ax_set_point(w, attr_position(), CGPoint { x: frame.0, y: frame.1 });
            ax_set_size(w, attr_size(), CGSize { w: frame.2, h: frame.3 });
        }
    }
}

/// Raise the window (keyboard follows for zoom keys): activate its app and
/// order the window front.
pub fn focus(id: &WindowId) {
    let Some((w, pid)) = resolve(id) else { return };
    unsafe {
        let ws = send(class("NSWorkspace"), "sharedWorkspace");
        let apps = send(ws, "runningApplications");
        let count: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(msg_fn());
        let n = count(apps, sel("count"));
        for i in 0..n {
            let app = send_index(apps, "objectAtIndex:", i);
            if send_ret_i32(app, "processIdentifier") == pid as i32 {
                let act: unsafe extern "C" fn(Id, Sel, u64) = std::mem::transmute(msg_fn());
                act(app, sel("activateWithOptions:"), 1); // activateIgnoringOtherWindows
                break;
            }
        }
        send(w, "raise");
    }
}

pub fn set_minimized(id: &WindowId, minimized: bool) {
    let Some((w, _)) = resolve(id) else { return };
    ax_set_bool(w, attr_minimized(), minimized);
}

pub fn set_fullscreen(id: &WindowId, fullscreen: bool) {
    let Some((w, _)) = resolve(id) else { return };
    ax_set_bool(w, attr_fullscreen(), fullscreen);
}

/// The placed window's own close button, for the gesture layer's intercept.
pub fn close_button(id: &WindowId) -> Option<(f64, f64, f64, f64)> {
    let (w, _) = resolve(id)?;
    let btn = ax_copy(w, attr_close_button())?;
    let p = ax_point(btn, attr_position())?;
    let s = ax_size(btn, attr_size())?;
    Some((p.x, p.y, s.0, s.1))
}

// ---- screen ----------------------------------------------------------------

/// NSScreen.visibleFrame of the phone display, as text: NSStringFromRect
/// keeps every return value scalar, so one path serves arm64 and x86_64.
fn screen_visible_frame(display_id: u32) -> Option<(f64, f64, f64, f64)> {
    unsafe {
        let screens = send(class("NSScreen"), "screens");
        let n = send_ret_i32(screens, "count");
        for i in 0..n {
            let screen = send_index(screens, "objectAtIndex:", i as isize);
            let dict = send(screen, "deviceDescription");
            let num = send_id(dict, "objectForKey:", ns_string("NSScreenNumber"));
            if send_ret_i32(num, "intValue") != display_id as i32 {
                continue;
            }
            let r = send_rect(screen, "visibleFrame");
            return Some((r.x, r.y, r.w, r.h));
        }
    }
    None
}

pub fn phone_display_area() -> Option<DisplayArea> {
    let id = crate::phone::display::mac::held_display_id()?;
    let b = unsafe { CGDisplayBounds(id) };
    let visible = screen_visible_frame(id).unwrap_or((b.x, b.y, b.w, b.h));
    // visibleFrame is bottom-left-origin; AX and CGDisplayBounds are
    // top-left-origin. Convert: y_top = bounds.y + (bounds.h - (vf.y + vf.h)).
    let y_top = b.y + (b.h - (visible.1 + visible.3));
    Some(DisplayArea { bounds: (b.x, b.y, b.w, b.h), usable: (visible.0, y_top, visible.2, visible.3) })
}

pub fn held_fit() -> Option<crate::phone::fit::Fit> {
    crate::phone::display::mac::held_fit()
}

/// Switch the phone display's step (the helper does the actual switching).
pub fn set_step_width(pts: u32) -> Result<u32, String> {
    let Some(id) = crate::phone::display::mac::held_display_id() else { return Err("phone display is not on".into()) };
    let Some(fit) = crate::phone::display::mac::held_fit() else { return Err("phone display is not on".into()) };
    crate::phone::display::mac::set_step(id, pts, &fit)?;
    Ok(pts)
}

/// The frontmost regular app's pid (the gesture layer and zoom use it).
pub fn frontmost_pid() -> Option<u32> {
    unsafe {
        let ws = send(class("NSWorkspace"), "sharedWorkspace");
        let app = send(ws, "frontmostApplication");
        if app.is_null() {
            return None;
        }
        let pid = send_ret_i32(app, "processIdentifier");
        if pid > 0 {
            Some(pid as u32)
        } else {
            None
        }
    }
}
