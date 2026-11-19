// Windows window mover: Win32 + DWM extended frame bounds.
//
// Coordinates are physical pixels of the virtual desktop. The process must
// be per-monitor DPI aware or GetWindowRect hands back virtualized sizes
// for windows on scaled monitors, so the mover sets that at first use.
//
// The window id carried through the records is "pid:hwnd" - stable while
// the window lives, and hwnd_of recovers the handle from it.

use super::{DisplayArea, Movable, WindowId};
use std::ffi::c_void;

type HWND = *mut c_void;
type HMONITOR = *mut c_void;
type HDC = *mut c_void;
type LPARAM = isize;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct RECT {
    pub(crate) l: i32,
    pub(crate) t: i32,
    pub(crate) r: i32,
    pub(crate) b: i32,
}

const SW_RESTORE: i32 = 9;
const SW_MINIMIZE: i32 = 6;
const SWP_NOZORDER: u32 = 0x4;
const SWP_NOACTIVATE: u32 = 0x10;
const GWL_STYLE: i32 = -16;
const WS_CAPTION: isize = 0x00C0_0000;
const DWMWA_EXTENDED_FRAME_BOUNDS: i32 = 9;
const DWMWA_CLOAKED: i32 = 14;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: Option<unsafe extern "system" fn(HWND, LPARAM) -> i32>, lparam: LPARAM) -> i32;
    fn IsWindowVisible(h: HWND) -> i32;
    fn IsIconic(h: HWND) -> i32;
    fn ShowWindow(h: HWND, cmd: i32) -> i32;
    fn GetForegroundWindow() -> HWND;
    fn SetForegroundWindow(h: HWND) -> i32;
    fn GetWindowTextLengthW(h: HWND) -> i32;
    fn GetWindowTextW(h: HWND, buf: *mut u16, max: i32) -> i32;
    fn GetWindowRect(h: HWND, r: *mut RECT) -> i32;
    fn SetWindowPos(h: HWND, after: HWND, x: i32, y: i32, w: i32, h2: i32, flags: u32) -> i32;
    fn GetWindowThreadProcessId(h: HWND, pid: *mut u32) -> u32;
    fn GetWindowLongPtrW(h: HWND, idx: i32) -> isize;
    fn EnumDisplayMonitors(dc: HDC, clip: *const c_void, cb: Option<unsafe extern "system" fn(HMONITOR, HDC, *mut RECT, LPARAM) -> i32>, lparam: LPARAM) -> i32;
    fn GetMonitorInfoW(m: HMONITOR, info: *mut MONITORINFO) -> i32;
    fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(h: HWND, attr: u32, out: *mut c_void, size: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn QueryFullProcessImageNameW(h: *mut c_void, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
}

#[repr(C)]
struct MONITORINFO {
    cb_size: u32,
    rc_monitor: RECT,
    rc_work: RECT,
    dw_flags: u32,
}

/// GetMonitorInfoW accepts either MONITORINFO or MONITORINFOEXW; the ExW
/// layout is a superset whose trailing szDevice names the monitor as
/// "\\.\DISPLAYn" - exactly the name winvdd's pool reports.
#[repr(C)]
struct MonitorInfoExW {
    cb_size: u32,
    rc_monitor: RECT,
    rc_work: RECT,
    dw_flags: u32,
    device: [u16; 32],
}

fn dpi_aware_once() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        unsafe {
            SetProcessDpiAwarenessContext(-4); // PER_MONITOR_AWARE_V2
        }
    });
}

fn title_of(h: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, buf.as_mut_ptr(), len as i32 + 1);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

fn app_name_of(pid: u32) -> String {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or("").trim_end_matches(".exe").to_string()
    }
}

/// Real visual bounds (DWM extended frame), which exclude the invisible
/// resize borders GetWindowRect includes.
fn frame_of(h: HWND) -> (f64, f64, f64, f64) {
    unsafe {
        let mut r = RECT::default();
        let ok = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32);
        if ok != 0 {
            GetWindowRect(h, &mut r);
        }
        (r.l as f64, r.t as f64, (r.r - r.l) as f64, (r.b - r.t) as f64)
    }
}

fn cloaked(h: HWND) -> bool {
    unsafe {
        let mut v: u32 = 0;
        DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, &mut v as *mut _ as *mut c_void, 4);
        v != 0
    }
}

unsafe extern "system" fn enum_cb(h: HWND, lparam: LPARAM) -> i32 {
    let wins = &mut *(lparam as *mut Vec<HWND>);
    wins.push(h);
    1
}

pub(crate) fn hwnd_of(id: &WindowId) -> Option<HWND> {
    // written as "pid:hwnd" with the hwnd in hex (id_of)
    let raw = id.split(':').nth(1)?;
    Some(isize::from_str_radix(raw, 16).ok()? as HWND)
}

fn id_of(h: HWND, pid: u32) -> WindowId {
    format!("{pid}:{:x}", h as isize)
}

fn movable(h: HWND, id: WindowId, pid: u32) -> Movable {
    let frame = frame_of(h);
    Movable {
        id,
        pid,
        title: title_of(h),
        app: app_name_of(pid),
        frame,
        minimized: unsafe { IsIconic(h) != 0 },
        // Fullscreen heuristics stay conservative: a maximized window on
        // another display is not fullscreen for our purposes; the real
        // maximized-on-phone-display case is handled by the frame itself.
        fullscreen: false,
    }
}

pub fn list_windows() -> Vec<Movable> {
    dpi_aware_once();
    unsafe {
        let mut handles: Vec<HWND> = Vec::new();
        EnumWindows(Some(enum_cb), &mut handles as *mut _ as LPARAM);
        let mut out = Vec::new();
        for h in handles {
            if IsWindowVisible(h) == 0 || cloaked(h) || title_of(h).is_empty() {
                continue;
            }
            if GetWindowLongPtrW(h, GWL_STYLE) & WS_CAPTION != WS_CAPTION {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, &mut pid);
            let frame = frame_of(h);
            if frame.2 < 40.0 || frame.3 < 40.0 {
                continue;
            }
            out.push(movable(h, id_of(h, pid), pid));
        }
        out
    }
}

pub fn focused_window() -> Option<Movable> {
    dpi_aware_once();
    unsafe {
        let h = GetForegroundWindow();
        if h.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        Some(movable(h, id_of(h, pid), pid))
    }
}

pub fn refresh(id: &WindowId) -> Option<Movable> {
    dpi_aware_once();
    let h = hwnd_of(id)?;
    unsafe {
        if IsWindowVisible(h) == 0 || title_of(h).is_empty() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        Some(movable(h, id.clone(), pid))
    }
}

pub fn set_frame(id: &WindowId, frame: (f64, f64, f64, f64)) {
    let Some(h) = hwnd_of(id) else { return };
    unsafe {
        SetWindowPos(h, std::ptr::null_mut(), frame.0 as i32, frame.1 as i32, frame.2 as i32, frame.3 as i32, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Bring the window to the foreground (keyboard follows for zoom keys).
pub fn focus(id: &WindowId) {
    let Some(h) = hwnd_of(id) else { return };
    unsafe {
        SetForegroundWindow(h);
    }
}

pub fn set_minimized(id: &WindowId, minimized: bool) {
    let Some(h) = hwnd_of(id) else { return };
    unsafe {
        ShowWindow(h, if minimized { SW_MINIMIZE } else { SW_RESTORE });
    }
}

pub fn set_fullscreen(_id: &WindowId, _fullscreen: bool) {
    // Windows-native fullscreen is rare among the windows we move; a
    // maximized window is un-maximized by the frame change itself.
}

/// The claimed phone monitor's area in pixels.
pub fn phone_display_area() -> Option<DisplayArea> {
    dpi_aware_once();
    let monitor = crate::phone::display::win::claimed_monitor()?;
    let mut ctx: (Vec<u16>, Option<(i32, i32, i32, i32, i32, i32, i32, i32)>) = (monitor.encode_utf16().chain([0]).collect(), None);
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(monitor_cb), &mut ctx as *mut _ as LPARAM);
    }
    let Some((mx, my, mx2, my2, wx, wy, wx2, wy2)) = ctx.1 else { return None };
    Some(DisplayArea {
        bounds: (mx as f64, my as f64, (mx2 - mx) as f64, (my2 - my) as f64),
        usable: (wx as f64, wy as f64, (wx2 - wx) as f64, (wy2 - wy) as f64),
    })
}

unsafe extern "system" fn monitor_cb(m: HMONITOR, _dc: HDC, _r: *mut RECT, lparam: LPARAM) -> i32 {
    let ctx = &mut *(lparam as *mut (Vec<u16>, Option<(i32, i32, i32, i32, i32, i32, i32, i32)>));
    let mut info = MonitorInfoExW { cb_size: std::mem::size_of::<MonitorInfoExW>() as u32, rc_monitor: RECT::default(), rc_work: RECT::default(), dw_flags: 0, device: [0; 32] };
    if GetMonitorInfoW(m, &mut info as *mut _ as *mut MONITORINFO) != 0 {
        let name = String::from_utf16_lossy(&info.device).trim_end_matches('\0').to_string();
        let wanted = String::from_utf16_lossy(&ctx.0).trim_end_matches('\0').to_string();
        if name == wanted {
            ctx.1 = Some((info.rc_monitor.l, info.rc_monitor.t, info.rc_monitor.r, info.rc_monitor.b, info.rc_work.l, info.rc_work.t, info.rc_work.r, info.rc_work.b));
        }
    }
    1
}

pub fn held_fit() -> Option<super::Fit> {
    crate::phone::display::win::held_fit_win()
}

/// Switch the phone display to another ladder step; returns the achieved
/// width in points (the driver snaps to its sampled mode table).
pub fn set_step_width(pts: u32) -> Result<u32, String> {
    let Some(fit) = crate::phone::display::win::held_fit_win() else { return Err("phone display is not on".into()) };
    let (w, _) = crate::phone::display::win::set_step(pts, &fit)?;
    Ok((w / 2).max(1))
}
