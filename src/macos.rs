// macOS backend — private CoreGraphics API (CGSConfigureDisplayEnabled), the same
// mechanism the classic DisableMonitor tool used. A disabled display vanishes from
// the system as if unplugged. Symbols are resolved at runtime via dlsym so a future
// macOS that drops them degrades to a clean error instead of failing to launch.

use crate::common::DisplayInfo;
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
                    let w = CGDisplayModeGetPixelWidth(mode);
                    let h = CGDisplayModeGetPixelHeight(mode);
                    let r = CGDisplayModeGetRefreshRate(mode);
                    CFRelease(mode);
                    let r = if r > 1.0 { format!(" @{}Hz", r.round() as i64) } else { String::new() };
                    format!("{w}x{h}{r}")
                };
                let b = CGDisplayBounds(id);
                DisplayInfo {
                    id: id.to_string(),
                    active: CGDisplayIsActive(id) != 0,
                    main: CGDisplayIsMain(id) != 0,
                    name: display_name(id),
                    detail: format!("{detail} ({},{})", b.x as i64, b.y as i64),
                }
            })
            .collect()
    }
}

pub fn set_enabled(id: &str, enable: bool) -> Result<(), String> {
    let Ok(id) = id.parse::<CGDirectDisplayID>() else {
        return Err(format!("'{}' is not a valid macOS display id (expected a number)", id));
    };
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
