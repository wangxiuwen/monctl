// Per-app font zoom: when a window lands on the phone display, its text is
// enlarged by sending the app's own zoom keys, and shrunk back on restore.
// Levels are remembered per app (zoom.json) so each app keeps its size.
//
// macOS sends ⌘+/⌘- to the app's process only (CGEventPostToPSN) - other
// apps are untouched, matching Snug. Windows delivers through SendInput to
// whatever has keyboard focus, which is only safe when the window being
// zoomed is the focused one, so the mover focuses first.
//
// Only apps whose ⌘= means "bigger text" are zoomed (terminals and code
// editors); browsers remember zoom per website and would leak it everywhere.

use super::json::Json;
use std::collections::BTreeMap;

/// Apps known to handle ⌘= as font size. Everything else is left alone.
pub const ELIGIBLE_MAC: &[&str] = &["Ghostty", "Terminal", "iTerm2", "iTerm", "Code", "Cursor", "Zed"];
pub const ELIGIBLE_WIN: &[&str] = &["WindowsTerminal", "wt", "Code", "Cursor", "Zed", "ConEmu64", "mintty"];
pub const MIN_LEVEL: i32 = -4;
pub const MAX_LEVEL: i32 = 8;

pub fn eligible(app: &str) -> bool {
    let list = if cfg!(target_os = "macos") { ELIGIBLE_MAC } else { ELIGIBLE_WIN };
    list.iter().any(|e| app.contains(e))
}

fn path() -> std::path::PathBuf {
    crate::common::state_path().with_file_name("phone-zoom.json")
}

pub fn load() -> BTreeMap<String, i32> {
    let Ok(text) = std::fs::read_to_string(path()) else { return BTreeMap::new() };
    let Ok(v) = super::json::parse(&text) else { return BTreeMap::new() };
    let mut out = BTreeMap::new();
    for (k, n) in v.members().unwrap_or(&[]) {
        if let Some(l) = n.as_num() {
            out.insert(k.clone(), l as i32);
        }
    }
    out
}

pub fn save(m: &BTreeMap<String, i32>) {
    let members: Vec<(String, Json)> = m.iter().map(|(k, v)| (k.clone(), Json::Num(f64::from(*v)))).collect();
    crate::common::write_atomic(&path(), &Json::Obj(members).write());
}

/// Remember the level an app was zoomed to.
pub fn remember(app: &str, level: i32) {
    let mut m = load();
    m.insert(app.to_string(), level.clamp(MIN_LEVEL, MAX_LEVEL));
    save(&m);
}

/// The remembered level for an app (0 = none).
pub fn level_of(app: &str) -> i32 {
    load().get(app).copied().unwrap_or(0)
}

/// Send `steps` zoom-in (or zoom-out when negative) keystrokes to the app's
/// window. macOS posts to the process; Windows sends to the focused window.
pub fn zoom(app: &str, steps: i32) {
    if steps == 0 || !eligible(app) {
        return;
    }
    #[cfg(target_os = "macos")]
    mac::post_zoom(steps);
    #[cfg(windows)]
    win::send_zoom(steps);
}

#[cfg(target_os = "macos")]
pub mod mac {
    use std::ffi::c_void;
    type Id = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, down: bool) -> Id;
        fn CGEventSetFlags(e: Id, flags: u64);
        fn CGEventPostToPid(pid: i32, e: Id);
        fn CGEventKeyboardSetUnicodeString(e: Id, len: u64, s: *const u16);
    }

    const CMD_FLAG: u64 = 1 << 20;
    const KEY_EQUAL: u16 = 0x2E; // = / + key
    const KEY_MINUS: u16 = 0x2D;

    /// One ⌘= (or ⌘-) keystroke pair, posted to the app's own process.
    pub fn post_zoom(steps: i32) {
        let Some(pid) = frontmost_pid() else { return };
        unsafe {
            for _ in 0..steps.abs() {
                let key = if steps > 0 { KEY_EQUAL } else { KEY_MINUS };
                let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), key, true);
                CGEventSetFlags(down, CMD_FLAG);
                CGEventPostToPid(pid, down);
                let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), key, false);
                CGEventSetFlags(up, CMD_FLAG);
                CGEventPostToPid(pid, up);
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
        }
    }

    fn frontmost_pid() -> Option<i32> {
        crate::phone::window::mac::frontmost_pid().map(|p| p as i32)
    }
}

#[cfg(windows)]
pub mod win {
    const VK_CONTROL: u16 = 0x11;
    const VK_EQUAL: u16 = 0xBB;
    const VK_MINUS: u16 = 0xBD;
    const KEYEVENTF_KEYUP: u32 = 2;

    /// Ctrl+= / Ctrl+- to the focused window. The mover focuses the window
    /// before calling; apps that need the real accelerator table (VS Code's
    /// webview) still receive it through SendInput.
    pub fn send_zoom(steps: i32) {
        unsafe extern "system" {
            fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
        }
        let vk = if steps > 0 { VK_EQUAL } else { VK_MINUS };
        for _ in 0..steps.abs() {
            unsafe {
                keybd_event(VK_CONTROL as u8, 0, 0, 0);
                keybd_event(vk as u8, 0, 0, 0);
                keybd_event(vk as u8, 0, KEYEVENTF_KEYUP, 0);
                keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    }
}
