// macOS backend — private CoreGraphics API (CGSConfigureDisplayEnabled), the same
// mechanism the classic DisableMonitor tool used. A disabled display vanishes from
// the system as if unplugged. Symbols are resolved at runtime via dlsym so a future
// macOS that drops them degrades to a clean error instead of failing to launch.

use crate::common::{DisplayInfo, Mode};
use std::ffi::{c_char, c_int, c_void, CStr, CString};

type CGDirectDisplayID = u32;
const K_CG_CONFIGURE_FOR_SESSION: i32 = 1;
const K_CF_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32) -> i32;
    fn CGDisplayIsActive(d: CGDirectDisplayID) -> c_int;
    fn CGDisplayIsMain(d: CGDirectDisplayID) -> c_int;
    fn CGDisplayBounds(d: CGDirectDisplayID) -> CGRect;
    fn CGDisplayCopyDisplayMode(d: CGDirectDisplayID) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(m: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(m: *const c_void) -> usize;
    fn CGDisplayModeGetRefreshRate(m: *const c_void) -> f64;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: i32) -> i32;
    fn CGDisplayCopyAllDisplayModes(d: CGDirectDisplayID, options: *const c_void) -> *mut c_void;
    fn CGDisplayModeGetWidth(m: *const c_void) -> usize;
    fn CGDisplayModeGetHeight(m: *const c_void) -> usize;
    fn CGDisplayModeIsUsableForDesktopGUI(m: *const c_void) -> bool;
    fn CGDisplayModeGetIODisplayModeID(m: *const c_void) -> i32;
    fn CGConfigureDisplayWithDisplayMode(config: *mut c_void, d: CGDirectDisplayID, mode: *const c_void, options: *const c_void) -> i32;
    fn CGConfigureDisplayOrigin(config: *mut c_void, d: CGDirectDisplayID, x: i32, y: i32) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(config: *mut c_void, d: CGDirectDisplayID, master: CGDirectDisplayID) -> i32;
    fn CGDisplayMirrorsDisplay(d: CGDirectDisplayID) -> CGDirectDisplayID;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    static kCGDisplayShowDuplicateLowResolutionModes: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut c_void);
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> *mut c_void;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, enc: u32) -> u8;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFDictionaryGetCount(dict: *const c_void) -> isize;
    fn CFDictionaryGetKeysAndValues(dict: *const c_void, keys: *mut *const c_void, vals: *mut *const c_void);
    fn CFDictionaryCreate(alloc: *const c_void, keys: *const *const c_void, vals: *const *const c_void, n: isize, kcb: *const c_void, vcb: *const c_void) -> *mut c_void;
    fn CFArrayGetCount(a: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

#[repr(C)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn symbol(name: &str) -> *mut c_void {
    // dlopen(NULL) → handle for the main program; dlsym on it searches the main
    // binary plus every library loaded at launch (CoreGraphics included).
    // NB: macOS RTLD_DEFAULT is (void*)-2, NOT NULL — passing NULL means
    // RTLD_MAIN_ONLY and would miss CoreGraphics symbols.
    let c = CString::new(name).unwrap();
    unsafe { dlsym(dlopen(std::ptr::null(), RTLD_NOW), c.as_ptr()) }
}

unsafe fn cf_to_string(cf: *const c_void) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 512];
    if CFStringGetCString(cf, buf.as_mut_ptr(), 512, K_CF_UTF8) != 0 {
        return Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
    }
    None
}

unsafe fn display_name(id: CGDirectDisplayID) -> String {
    let create_info_sym = symbol("CoreDisplay_DisplayCreateInfoDictionary");
    if create_info_sym.is_null() {
        return format!("Display {id}");
    }
    let create_info: unsafe extern "C" fn(CGDirectDisplayID) -> *mut c_void = unsafe { std::mem::transmute(create_info_sym) };
    let dict = create_info(id);
    if dict.is_null() {
        return format!("Display {id}");
    }

    let mut name = None;
    for key in ["DisplayProductName", "kDisplayProductName"] {
        let key_cf = CFStringCreateWithCString(std::ptr::null(), CString::new(key).unwrap().as_ptr(), K_CF_UTF8);
        let val = CFDictionaryGetValue(dict, key_cf);
        CFRelease(key_cf);
        if val.is_null() {
            continue;
        }
        if CFGetTypeID(val) == CFStringGetTypeID() {
            name = cf_to_string(val);
        } else if CFGetTypeID(val) == CFDictionaryGetTypeID() {
            // localized dictionary of names: take the first non-empty string value
            let n = CFDictionaryGetCount(val).max(0) as usize;
            if n > 0 {
                let mut vals: Vec<*const c_void> = vec![std::ptr::null(); n];
                let mut keys: Vec<*const c_void> = vec![std::ptr::null(); n];
                CFDictionaryGetKeysAndValues(val, keys.as_mut_ptr(), vals.as_mut_ptr());
                for v in vals {
                    if CFGetTypeID(v) == CFStringGetTypeID() {
                        if let Some(s) = cf_to_string(v).filter(|s| !s.is_empty()) {
                            name = Some(s);
                            break;
                        }
                    }
                }
            }
        }
        if name.as_deref().is_some_and(|s| !s.is_empty()) {
            break;
        }
    }
    CFRelease(dict);
    name.filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Display {id}"))
}

pub fn list() -> Vec<DisplayInfo> {
    unsafe {
        let mut count: u32 = 0;
        CGGetOnlineDisplayList(0, std::ptr::null_mut(), &mut count);
        if count == 0 {
            return vec![];
        }
        let mut ids = vec![0u32; count as usize];
        CGGetOnlineDisplayList(count, ids.as_mut_ptr(), &mut count);
        ids.truncate(count as usize);

        ids.into_iter()
            .map(|id| {
                let mode = CGDisplayCopyDisplayMode(id);
                let detail = if mode.is_null() {
                    "n/a".to_string()
                } else {
                    // what the desktop looks like, not the 2x backing store
                    let w = CGDisplayModeGetWidth(mode);
                    let h = CGDisplayModeGetHeight(mode);
                    let hidpi = if CGDisplayModeGetPixelWidth(mode) > w { " HiDPI" } else { "" };
                    let r = CGDisplayModeGetRefreshRate(mode);
                    CFRelease(mode);
                    let r = if r > 1.0 { format!(" @{}Hz", r.round() as i64) } else { String::new() };
                    format!("{w}x{h}{hidpi}{r}")
                };
                let b = CGDisplayBounds(id);
                let master = CGDisplayMirrorsDisplay(id);
                let mirror = if master != 0 { format!(" mirror of {master}") } else { String::new() };
                DisplayInfo {
                    id: id.to_string(),
                    active: CGDisplayIsActive(id) != 0,
                    main: CGDisplayIsMain(id) != 0,
                    name: display_name(id),
                    detail: format!("{detail} ({},{}){mirror}", b.x as i64, b.y as i64),
                    mirror_of: (master != 0).then(|| master.to_string()),
                }
            })
            .collect()
    }
}

fn parse_id(id: &str) -> Result<CGDirectDisplayID, String> {
    id.parse().map_err(|_| format!("'{id}' is not a valid macOS display id (expected a number)"))
}

/// Run one display reconfiguration transaction.
fn configure(option: i32, f: impl FnOnce(*mut c_void) -> Result<(), String>) -> Result<(), String> {
    unsafe {
        let mut config: *mut c_void = std::ptr::null_mut();
        let err = CGBeginDisplayConfiguration(&mut config);
        if err != 0 {
            return Err(format!("CGBeginDisplayConfiguration failed: {err}"));
        }
        if let Err(e) = f(config) {
            CGCancelDisplayConfiguration(config);
            return Err(e);
        }
        let err = CGCompleteDisplayConfiguration(config, option);
        if err != 0 {
            return Err(format!("CGCompleteDisplayConfiguration failed: {err}"));
        }
    }
    Ok(())
}

// Resolution/arrangement changes persist like System Settings does; only
// enable/disable stays session-scoped (a reboot must never leave a dead screen).
const K_CG_CONFIGURE_PERMANENTLY: i32 = 2;

/// Every desktop-usable mode, including the HiDPI ("looks like") variants
/// System Settings hides behind "Show all resolutions". Caller releases the array.
unsafe fn copy_modes(id: CGDirectDisplayID) -> (*mut c_void, Vec<(*const c_void, Mode)>) {
    let keys = [kCGDisplayShowDuplicateLowResolutionModes];
    let vals = [kCFBooleanTrue];
    let opts = CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), vals.as_ptr(), 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    let arr = CGDisplayCopyAllDisplayModes(id, opts);
    CFRelease(opts);
    if arr.is_null() {
        return (arr, vec![]);
    }
    let cur = CGDisplayCopyDisplayMode(id);
    let cur_key = if cur.is_null() {
        None
    } else {
        let k = (CGDisplayModeGetIODisplayModeID(cur), CGDisplayModeGetWidth(cur), CGDisplayModeGetPixelWidth(cur));
        CFRelease(cur);
        Some(k)
    };
    let mut out = vec![];
    for i in 0..CFArrayGetCount(arr) {
        let m = CFArrayGetValueAtIndex(arr, i);
        if !CGDisplayModeIsUsableForDesktopGUI(m) {
            continue;
        }
        let key = (CGDisplayModeGetIODisplayModeID(m), CGDisplayModeGetWidth(m), CGDisplayModeGetPixelWidth(m));
        out.push((
            m,
            Mode {
                width: CGDisplayModeGetWidth(m) as u32,
                height: CGDisplayModeGetHeight(m) as u32,
                pixel_width: CGDisplayModeGetPixelWidth(m) as u32,
                pixel_height: CGDisplayModeGetPixelHeight(m) as u32,
                hz: CGDisplayModeGetRefreshRate(m).round() as u32,
                current: Some(key) == cur_key,
            },
        ));
    }
    (arr, out)
}

pub fn modes(id: &str) -> Result<Vec<Mode>, String> {
    let id = parse_id(id)?;
    unsafe {
        let (arr, modes) = copy_modes(id);
        if arr.is_null() {
            return Err(format!("display {id} reports no modes"));
        }
        let out = modes.into_iter().map(|(_, m)| m).collect();
        CFRelease(arr);
        Ok(out)
    }
}

pub fn set_mode(id: &str, mode: &Mode) -> Result<(), String> {
    let id = parse_id(id)?;
    unsafe {
        let (arr, modes) = copy_modes(id);
        let Some((m, _)) = modes.iter().find(|(_, m)| Mode { current: false, ..m.clone() } == Mode { current: false, ..mode.clone() }) else {
            if !arr.is_null() {
                CFRelease(arr);
            }
            return Err(format!("display {id} has no mode {}", mode.describe()));
        };
        let r = configure(K_CG_CONFIGURE_PERMANENTLY, |c| match CGConfigureDisplayWithDisplayMode(c, id, *m, std::ptr::null()) {
            0 => Ok(()),
            e => Err(format!("CGConfigureDisplayWithDisplayMode failed: {e}")),
        });
        CFRelease(arr);
        r
    }
}

/// Make `id` the main display (menu bar + Dock): macOS treats whichever
/// display sits at origin (0,0) as main, so shift the whole layout.
pub fn set_main(id: &str) -> Result<(), String> {
    let target = parse_id(id)?;
    let displays = list();
    let origin = unsafe { CGDisplayBounds(target) };
    configure(K_CG_CONFIGURE_PERMANENTLY, |c| {
        for d in displays.iter().filter(|d| d.active) {
            let did: CGDirectDisplayID = d.id.parse().unwrap_or(0);
            if unsafe { CGDisplayMirrorsDisplay(did) } != 0 {
                continue; // mirrors follow their master
            }
            let b = unsafe { CGDisplayBounds(did) };
            let (x, y) = ((b.x - origin.x) as i32, (b.y - origin.y) as i32);
            match unsafe { CGConfigureDisplayOrigin(c, did, x, y) } {
                0 => {}
                e => return Err(format!("CGConfigureDisplayOrigin({did}) failed: {e}")),
            }
        }
        Ok(())
    })
}

/// Show `master`'s picture on `id` (None = stop mirroring).
pub fn set_mirror(id: &str, master: Option<&str>) -> Result<(), String> {
    let id = parse_id(id)?;
    let master = match master {
        Some(m) => parse_id(m)?,
        None => 0, // kCGNullDirectDisplay
    };
    if master == id {
        return Err("a display cannot mirror itself".into());
    }
    configure(K_CG_CONFIGURE_PERMANENTLY, |c| match unsafe { CGConfigureDisplayMirrorOfDisplay(c, id, master) } {
        0 => Ok(()),
        e => Err(format!("CGConfigureDisplayMirrorOfDisplay failed: {e}")),
    })
}

pub fn main_id() -> String {
    unsafe { CGMainDisplayID() }.to_string()
}

pub fn set_enabled(id: &str, enable: bool) -> Result<(), String> {
    let id = parse_id(id)?;
    let sym = symbol("CGSConfigureDisplayEnabled");
    if sym.is_null() {
        return Err("CGSConfigureDisplayEnabled not found on this macOS version".into());
    }
    let cgs_set: unsafe extern "C" fn(*mut c_void, CGDirectDisplayID, bool) -> i32 = unsafe { std::mem::transmute(sym) };

    unsafe {
        let mut config: *mut c_void = std::ptr::null_mut();
        let err = CGBeginDisplayConfiguration(&mut config);
        if err != 0 {
            return Err(format!("CGBeginDisplayConfiguration failed: {err}"));
        }
        let err = cgs_set(config, id, enable);
        if err != 0 {
            CGCancelDisplayConfiguration(config);
            return Err(format!("CGSConfigureDisplayEnabled failed: {err}"));
        }
        // forSession: survives this process, resets on logout/reboot (sane default)
        let err = CGCompleteDisplayConfiguration(config, K_CG_CONFIGURE_FOR_SESSION);
        if err != 0 {
            return Err(format!("CGCompleteDisplayConfiguration failed: {err}"));
        }
    }
    Ok(())
}
