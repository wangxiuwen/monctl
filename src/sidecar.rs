// monctl sidecar — Apple's own iPad-as-a-display (Sidecar), from the command
// line and the menu bar: wired over USB or wireless, the best picture and
// latency an iPad gets, nothing to install. (An iPad's browser can't reach
// the Mac over a USB cable, so this is how "wired iPad" works.)
//
// Private SidecarCore API, resolved at runtime: SidecarDisplayManager
// (+sharedManager, -devices, -connectedDevices, -connectToDevice:completion:,
// -disconnectFromDevice:completion:) and SidecarDevice (-name).

use crate::common::fail;
use crate::virt::{class, msg_fn, send, Id};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    static _NSConcreteGlobalBlock: c_void;
    static kCFRunLoopDefaultMode: Id;
    fn CFRunLoopRunInMode(mode: Id, seconds: f64, return_after_source_handled: u8) -> i32;
    // same signatures as macos.rs declares them (rustc checks they agree)
    fn CFArrayGetCount(a: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
}

fn manager() -> Result<Id, String> {
    unsafe {
        dlopen(c"/System/Library/PrivateFrameworks/SidecarCore.framework/SidecarCore".as_ptr(), 2);
        let cls = class("SidecarDisplayManager");
        if cls.is_null() {
            return Err("Sidecar is not available on this Mac".into());
        }
        let m = send(cls, "sharedManager");
        if m.is_null() {
            return Err("Sidecar is not available on this Mac".into());
        }
        Ok(m)
    }
}

unsafe fn string(ns: Id) -> String {
    if ns.is_null() {
        return String::new();
    }
    let p = send(ns, "UTF8String") as *const c_char;
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// (name, device object) of an NSArray of SidecarDevice.
unsafe fn named(list: Id) -> Vec<(String, Id)> {
    if list.is_null() {
        return vec![];
    }
    (0..CFArrayGetCount(list)).map(|i| CFArrayGetValueAtIndex(list, i) as Id).map(|d| (string(send(d, "name")), d)).collect()
}

/// Devices show up asynchronously: let discovery run (on this thread's run
/// loop) until some appear or `wait` is up.
fn devices(m: Id, wait: Duration) -> Vec<(String, Id)> {
    let until = Instant::now() + wait;
    loop {
        let d = unsafe { named(send(m, "devices")) };
        if !d.is_empty() || Instant::now() >= until {
            return d;
        }
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, 0) };
    }
}

// ---- completion block: void (^)(NSError *) ----

#[repr(C)]
struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*const Block, Id),
    descriptor: *const Descriptor,
}

#[repr(C)]
struct Descriptor {
    reserved: usize,
    size: usize,
}

static DESCRIPTOR: Descriptor = Descriptor { reserved: 0, size: std::mem::size_of::<Block>() };
/// 0 = pending, 1 = done, 2 = failed (one Sidecar call per process run)
static RESULT: AtomicI32 = AtomicI32::new(0);
static ERROR: Mutex<String> = Mutex::new(String::new());

unsafe extern "C" fn completed(_: *const Block, error: Id) {
    if error.is_null() {
        RESULT.store(1, Ordering::SeqCst);
    } else {
        *ERROR.lock().unwrap() = string(send(error, "localizedDescription"));
        RESULT.store(2, Ordering::SeqCst);
    }
}

/// Call -connectToDevice:completion: / -disconnectFromDevice:completion: and
/// wait for the completion (delivered through the run loop).
fn call(m: Id, selector: &str, device: Id) -> Result<(), String> {
    let block: &'static Block = Box::leak(Box::new(Block {
        isa: unsafe { &_NSConcreteGlobalBlock },
        flags: 1 << 28, // BLOCK_IS_GLOBAL: Block_copy keeps our pointer
        reserved: 0,
        invoke: completed,
        descriptor: &DESCRIPTOR,
    }));
    unsafe {
        let f: unsafe extern "C" fn(Id, Id, Id, *const Block) = std::mem::transmute(msg_fn());
        f(m, crate::virt::sel(selector), device, block);
    }
    let until = Instant::now() + Duration::from_secs(30);
    while RESULT.load(Ordering::SeqCst) == 0 {
        if Instant::now() >= until {
            return Err("Sidecar did not answer within 30 s".into());
        }
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, 0) };
    }
    match RESULT.load(Ordering::SeqCst) {
        1 => Ok(()),
        _ => Err(ERROR.lock().unwrap().clone()),
    }
}

pub fn run(args: &[String]) -> ! {
    let m = manager().unwrap_or_else(|e| fail(&e));
    let wanted = args.join(" ");
    match wanted.as_str() {
        "" | "list" => {
            let all = devices(m, Duration::from_secs(3));
            let connected: Vec<String> = unsafe { named(send(m, "connectedDevices")) }.into_iter().map(|d| d.0).collect();
            if all.is_empty() {
                println!("no iPad found for Sidecar - it must be signed in to the same Apple ID, with Bluetooth/Wi-Fi on or a USB cable");
            }
            for (name, _) in &all {
                println!("{}{name}", if connected.contains(name) { "* " } else { "  " });
            }
            if !all.is_empty() {
                println!("(* = connected; monctl sidecar NAME connects, monctl sidecar off disconnects)");
            }
        }
        "off" | "disconnect" => {
            let connected = unsafe { named(send(m, "connectedDevices")) };
            if connected.is_empty() {
                println!("- no iPad is connected");
            }
            for (name, d) in connected {
                RESULT.store(0, Ordering::SeqCst);
                call(m, "disconnectFromDevice:completion:", d).unwrap_or_else(|e| fail(&format!("disconnecting {name}: {e}")));
                println!("disconnected {name}");
            }
        }
        name => {
            let all = devices(m, Duration::from_secs(5));
            let found = all.iter().find(|d| d.0 == name).or_else(|| all.iter().find(|d| d.0.to_lowercase().contains(&name.to_lowercase())));
            let Some((name, d)) = found else {
                let names: Vec<&str> = all.iter().map(|d| d.0.as_str()).collect();
                fail(&format!("no iPad named '{name}' (found: {})", if names.is_empty() { "none".to_string() } else { names.join(", ") }));
            };
            call(m, "connectToDevice:completion:", *d).unwrap_or_else(|e| fail(&format!("connecting {name}: {e}")));
            println!("connected {name} - it is a display now (monctl list)");
        }
    }
    std::process::exit(0)
}
