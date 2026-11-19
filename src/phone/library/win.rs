// Windows: the app library as a plain GDI list window on the phone display.
// The iPhone-style web page is macOS-only for now (WebKit); this list shows
// the running apps and opens the focused one on tap. Installed-app scanning
// (Start Menu .lnk) is a later refinement - until then this window says so.

use super::{App, Catalog};
use std::os::windows::process::CommandExt;
use std::ffi::c_void;
use std::sync::Mutex;

type HWND = *mut c_void;
type LPARAM = isize;
type WPARAM = usize;
type LRESULT = isize;

const WM_PAINT: u32 = 0xF;
const WM_DESTROY: u32 = 0x2;
const WM_LBUTTONUP: u32 = 0x202;
const WM_CLOSE: u32 = 0x10;
const CS_HREDRAW: u32 = 0x2;
const CS_VREDRAW: u32 = 0x1;
const COLOR_WINDOW: i32 = 5;
const FONT_HEIGHT: i32 = -28;
const ROW_H: i32 = 64;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RECT {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

#[repr(C)]
#[derive(Default)]
struct Msg {
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    pt: Point,
    time: u32,
    extra: [u64; 3],
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    instance: *mut c_void,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
#[derive(Default)]
struct PaintStruct {
    dc: *mut c_void,
    erased: bool,
    rc_paint: Rect,
    restore: bool,
    inc_update: bool,
    scroll_exclude: Rect,
    window: HWND,
}

static APP_LIST: Mutex<Vec<App>> = Mutex::new(Vec::new());

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(wc: *const WndClassW) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: *mut c_void, inst: *mut c_void, param: *mut c_void) -> HWND;
    fn DefWindowProcW(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn ShowWindow(h: HWND, cmd: i32) -> i32;
    fn GetMessageW(msg: *mut Msg, h: *mut c_void, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> i32;
    fn BeginPaint(h: HWND, ps: *mut PaintStruct) -> *mut c_void;
    fn EndPaint(h: HWND, ps: *const PaintStruct) -> i32;
    fn FillRect(dc: *mut c_void, r: *const Rect, brush: *mut c_void) -> i32;
    fn SetTextColor(dc: *mut c_void, color: u32) -> u32;
    fn SetBkMode(dc: *mut c_void, mode: i32) -> i32;
    fn TextOutW(dc: *mut c_void, x: i32, y: i32, text: *const u16, len: i32) -> i32;
    fn CreateFontW(size: i32, w: i32, e: i32, p: i32, weight: i32, italic: u32, u: u32, s: u32, charset: u32, out: u32, clip: u32, quality: u32, pitch: u32, face: *const u16) -> *mut c_void;
    fn SelectObject(dc: *mut c_void, obj: *mut c_void) -> *mut c_void;
    fn DeleteObject(obj: *mut c_void) -> i32;
    fn GetCursorPos(p: *mut Point) -> i32;
    fn PostQuitMessage(code: i32);
    fn GetSysColorBrush(i: i32) -> *mut c_void;
    fn InvalidateRect(h: HWND, r: *const Rect, erase: bool) -> i32;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(color: u32) -> *mut c_void;
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    use std::io::Write;
        match msg {
        WM_PAINT => {
            let mut ps = PaintStruct::default();
            let dc = BeginPaint(h, &mut ps);
            let bg = CreateSolidBrush(0x00000000);
            FillRect(dc, &ps.rc_paint, bg);
            DeleteObject(bg);
            let font = CreateFontW(FONT_HEIGHT, 0, 0, 0, 500, 0, 0, 0, 1, 0, 0, 2, 0, std::ptr::null());
            let old = SelectObject(dc, font);
            SetBkMode(dc, 1);
            SetTextColor(dc, 0x00FFFFFF); // white (BGR)
            let list = APP_LIST.lock().unwrap().clone();
            for (i, app) in list.iter().enumerate() {
                let text = format!("{}\0", app.name);
                let wide: Vec<u16> = text.encode_utf16().collect();
                TextOutW(dc, 24, 20 + i as i32 * ROW_H, wide.as_ptr(), (wide.len() - 1) as i32);
            }
            SelectObject(dc, old);
            DeleteObject(font);
            EndPaint(h, &ps);
            0
        }
        WM_LBUTTONUP => {
            let y = ((l >> 16) & 0xFFFF) as i16 as i32;
            let list = APP_LIST.lock().unwrap().clone();
            let idx = (y - 20) / ROW_H;
            if idx >= 0 && (idx as usize) < list.len() {
                let name = list[idx as usize].name.clone();
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &name])
                        .creation_flags(0x08000000)
                        .status();
                });
            }
            0
        }
        WM_CLOSE => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(h, msg, w, l),
    }
}

pub struct WinCatalog;

impl Catalog for WinCatalog {
    /// Running apps only: their process image names, which `open` can start
    /// via file association and `quit` can taskkill. Installed-but-not-running
    /// scanning (Start Menu .lnk) is a later refinement.
    fn apps(&self) -> Vec<App> {
        crate::phone::window::win::list_windows()
            .into_iter()
            .map(|w| App { name: if w.app.is_empty() { w.title.clone() } else { w.app.clone() }, pid: w.pid, running: true })
            .collect()
    }

    fn open(&self, name: &str) -> Result<(), String> {
        let status = std::process::Command::new("cmd")
            .args(["/C", "start", "", name])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("start {name} failed"))
        }
    }

    fn quit(&self, name: &str) -> Result<(), String> {
        // taskkill by image name; a real quit-by-grace comes with the
        // process-based catalog refinement.
        let status = std::process::Command::new("taskkill")
            .args(["/IM", &format!("{name}.exe")])
            .creation_flags(0x08000000)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("no running {name}.exe"))
        }
    }
}

/// Show the list window filling the phone display's usable area.
pub fn show() -> Result<(), String> {
    let Some(area) = crate::phone::window::phone_display_area() else { return Err("phone display is not on".into()) };
    let _ = &area;
    unsafe {
        let class_name: Vec<u16> = "MonctlPhoneLibrary\0".encode_utf16().collect();
        let wc = WndClassW {
            style: CS_HREDRAW | CS_VREDRAW,
            wnd_proc: Some(wnd_proc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            instance: std::ptr::null_mut(),
            icon: std::ptr::null_mut(),
            cursor: std::ptr::null_mut(),
            background: GetSysColorBrush(COLOR_WINDOW),
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
        };
        RegisterClassW(&wc);
        let title: Vec<u16> = "monctl 手机屏\0".encode_utf16().collect();
        // WS_POPUP | WS_VISIBLE, on the phone display's usable rect
        let h = CreateWindowExW(0x80000, class_name.as_ptr(), title.as_ptr(), 0x9000_0000, area.usable.0 as i32, area.usable.1 as i32, area.usable.2 as i32, (area.usable.3 - 56.0) as i32, std::ptr::null_mut(), std::ptr::null_mut(), wc.instance, std::ptr::null_mut());
        if h.is_null() {
            return Err(format!("CreateWindowExW: {}", std::io::Error::last_os_error()));
        }
        ShowWindow(h, 5);
        start_server();
        let mut msg = Msg::default();
        loop {
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn start_server() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let catalog = WinCatalog;
        *APP_LIST.lock().unwrap() = catalog.apps();
        std::thread::spawn(move || {
            let _ = super::run_server(catalog);
        });
    });
}
