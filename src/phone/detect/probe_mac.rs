// macOS probes behind the detectors: on-screen window owners through
// CGWindowList (plain C), and running processes through pgrep.
//
// Window NAMES need Screen Recording permission on recent macOS, but OWNER
// names and geometry do not - owner matching is what the detectors need.

use std::ffi::c_void;

type Id = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative: *mut c_void) -> Id; // CFArray of CFDictionary
    fn CFArrayGetCount(a: Id) -> isize;
    fn CFArrayGetValueAtIndex(a: Id, i: isize) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryGetValue(d: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const i8, enc: u32) -> Id;
    fn CFStringGetCString(s: Id, buf: *mut i8, size: isize, enc: u32) -> bool;
    fn CFNumberGetValue(n: *const c_void, t: isize, out: *mut c_void) -> bool;
}

const LIST_ON_SCREEN_ONLY: u32 = 1;
const UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT64: isize = 4;

fn cfstr(s: &str) -> Id {
    let c = std::ffi::CString::new(s.replace('\0', "")).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
}

fn cf_to_string(v: *const c_void) -> String {
    if v.is_null() {
        return String::new();
    }
    unsafe {
        let mut buf = [0i8; 512];
        if CFStringGetCString(v as Id, buf.as_mut_ptr(), 512, UTF8) {
            CStr_to_string(buf.as_ptr())
        } else {
            String::new()
        }
    }
}

fn CStr_to_string(p: *const i8) -> String {
    unsafe { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
}

/// On-screen window owners (app names) right now.
pub fn on_screen_owners() -> Vec<String> {
    unsafe {
        let arr = CGWindowListCopyWindowInfo(LIST_ON_SCREEN_ONLY, std::ptr::null_mut());
        if arr.is_null() {
            return vec![];
        }
        let n = CFArrayGetCount(arr);
        let mut out = Vec::new();
        for i in 0..n {
            let dict = CFArrayGetValueAtIndex(arr, i);
            if dict.is_null() {
                continue;
            }
            let owner = CFDictionaryGetValue(dict, cfstr("kCGWindowOwnerName") as *const c_void);
            if !owner.is_null() {
                out.push(cf_to_string(owner));
            }
        }
        out
    }
}

/// Whether an on-screen window's owner matches (case-insensitive substring).
pub fn owner_on_screen(substr: &str) -> bool {
    let needle = substr.to_ascii_lowercase();
    on_screen_owners().iter().any(|o| o.to_ascii_lowercase().contains(&needle))
}

/// Whether a process with this exact name is running (pgrep -x).
pub fn process_running(exact: &str) -> bool {
    let out = std::process::Command::new("/usr/bin/pgrep")
        .arg("-x")
        .arg(exact)
        .output();
    matches!(out, Ok(o) if o.status.success() && !o.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_do_not_crash_off_session() {
        // In a headless SSH session there may be zero on-screen windows;
        // the probes must still return cleanly.
        let _ = on_screen_owners();
        assert!(!process_running("definitely-not-a-process-xyz"));
    }
}
