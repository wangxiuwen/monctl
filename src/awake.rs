// monctl awake — prevent the machine from going to sleep for a while.
// Caffeinate-style: a detached holder process owns an OS-level power assertion
// (macOS IOKit assertion / Windows SetThreadExecutionState / systemd-inhibit),
// heartbeats a small JSON file, and exits when the deadline passes or a
// release flag appears — so `awake` commands are fire-and-forget.

use crate::common::{json_parse_object, json_write_object};
use std::collections::BTreeMap;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HOLDER_POLL_SECS: u64 = 2;
const STALE_AFTER_SECS: u64 = 15;
const FOREVER_SECS: u64 = 10 * 365 * 24 * 3600;

struct Hold {
    holder_pid: u32,
    deadline: u64,
    heartbeat: u64,
    display: bool,
    release: bool,
    requested: u64, // originally requested duration (0 = until released); tray uses it to tick the active menu item
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read_hold() -> Option<Hold> {
    let raw = std::fs::read_to_string(crate::common::awake_path()).ok()?;
    let map = json_parse_object(&raw)?;
    Some(Hold {
        holder_pid: map.get("holder_pid")?.parse().ok()?,
        deadline: map.get("deadline")?.parse().ok()?,
        heartbeat: map.get("heartbeat")?.parse().ok()?,
        display: map.get("display").map(|s| s == "1").unwrap_or(false),
        release: map.get("release").map(|s| s == "1").unwrap_or(false),
        requested: map.get("requested").and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

fn write_hold(h: &Hold) {
    let mut map = BTreeMap::new();
    map.insert("holder_pid".to_string(), h.holder_pid.to_string());
    map.insert("deadline".to_string(), h.deadline.to_string());
    map.insert("heartbeat".to_string(), h.heartbeat.to_string());
    map.insert("display".to_string(), if h.display { "1" } else { "0" }.to_string());
    map.insert("release".to_string(), if h.release { "1" } else { "0" }.to_string());
    map.insert("requested".to_string(), h.requested.to_string());
    let path = crate::common::awake_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, json_write_object(&map));
}

fn remove_hold_file() {
    let _ = std::fs::remove_file(crate::common::awake_path());
}

fn is_alive(h: &Hold) -> bool {
    now().saturating_sub(h.heartbeat) <= STALE_AFTER_SECS
}

fn fmt_remaining(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// "2h", "90m", "45s", "2h30m", or plain seconds.
fn parse_duration(s: &str) -> Option<u64> {
    if let Ok(secs) = s.parse::<u64>() {
        return Some(secs);
    }
    let mut total = 0u64;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            total += match c {
                'h' | 'H' => n.checked_mul(3600)?,
                'm' | 'M' => n.checked_mul(60)?,
                's' | 'S' => n,
                _ => return None,
            };
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(total)
}

// ---- platform inhibition ----

#[cfg(target_os = "macos")]
mod inhibit {
    use std::ffi::{c_char, c_void};

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(t: *const c_void, level: u32, d: *const c_void, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, enc: u32) -> *mut c_void;
        fn CFRelease(cf: *mut c_void);
    }
    const K_CF_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    fn make_assertion(kind: &str) -> u32 {
        unsafe {
            let t = CFStringCreateWithCString(std::ptr::null(), CString::new(kind).unwrap().as_ptr(), K_CF_UTF8);
            let d = CFStringCreateWithCString(std::ptr::null(), c"monctl".as_ptr(), K_CF_UTF8);
            let mut id = 0u32;
            IOPMAssertionCreateWithName(t, K_IOPM_ASSERTION_LEVEL_ON, d, &mut id);
            CFRelease(t);
            CFRelease(d);
            id
        }
    }

    use std::ffi::CString;

    pub struct Inhibit {
        ids: Vec<u32>,
    }

    impl Inhibit {
        pub fn acquire(display: bool) -> Inhibit {
            // PreventUserIdleSystemSleep: idle timeout sleep
            // PreventSystemSleep: lid-close/clamshell sleep while on AC power
            //                     (the caffeinate -s behaviour, honoured on AC only)
            let mut ids = vec![
                make_assertion("PreventUserIdleSystemSleep"),
                make_assertion("PreventSystemSleep"),
            ];
            if display {
                ids.push(make_assertion("PreventUserIdleDisplaySleep"));
            }
            Inhibit { ids }
        }
    }

    impl Drop for Inhibit {
        fn drop(&mut self) {
            for id in &self.ids {
                unsafe { IOPMAssertionRelease(*id) };
            }
        }
    }
}

#[cfg(windows)]
mod inhibit {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    pub struct Inhibit;

    impl Inhibit {
        pub fn acquire(display: bool) -> Inhibit {
            let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
            if display {
                flags |= ES_DISPLAY_REQUIRED;
            }
            unsafe { SetThreadExecutionState(flags) };
            Inhibit
        }
    }

    impl Drop for Inhibit {
        fn drop(&mut self) {
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

#[cfg(target_os = "linux")]
mod inhibit {
    use std::process::{Child, Command, Stdio};

    pub struct Inhibit {
        child: Option<Child>,
    }

    impl Inhibit {
        pub fn acquire(display: bool) -> Inhibit {
            // display sleep is X11/screen-saver territory; we inhibit the system only
            let _ = display;
            let child = Command::new("systemd-inhibit")
                .args(["--what=sleep:idle", "--mode=block", "sleep", "315360000"])
                .stdin(Stdio::null())
                .spawn()
                .ok();
            Inhibit { child }
        }
    }

    impl Drop for Inhibit {
        fn drop(&mut self) {
            if let Some(c) = &mut self.child {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

// ---- holder process ----

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid as i32, 15); // SIGTERM
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

fn spawn_holder(display: bool) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(["awake", "--hold"]);
    if display {
        cmd.arg("--display");
    }
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // detach from our terminal's signals
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    let child = cmd.spawn().map_err(|e| format!("cannot start holder process: {e}"))?;
    Ok(child.id())
}

fn holder_loop(display: bool) -> ! {
    let _inhibit = inhibit::Inhibit::acquire(display);
    loop {
        thread::sleep(Duration::from_secs(HOLDER_POLL_SECS));
        match read_hold() {
            Some(mut h) if !h.release && now() < h.deadline => {
                h.heartbeat = now();
                write_hold(&h);
            }
            _ => break,
        }
    }
    remove_hold_file();
    std::process::exit(0);
}

// ---- user-facing commands ----

fn active_hold() -> Option<Hold> {
    let h = read_hold()?;
    if h.release || !is_alive(&h) {
        remove_hold_file();
        return None;
    }
    Some(h)
}

fn status() {
    match active_hold() {
        None => println!("awake: no hold active"),
        Some(h) => {
            let remaining = h.deadline.saturating_sub(now());
            let tail = if h.deadline >= now() + FOREVER_SECS - 60 {
                String::new()
            } else {
                format!(", {} left", fmt_remaining(remaining))
            };
            println!(
                "awake: holding{}{} [holder pid {}]",
                tail,
                if h.display { ", display kept on too" } else { "" },
                h.holder_pid
            );
        }
    }
}

fn hold(secs: u64, display: bool) {
    let deadline = now() + secs;
    if let Some(existing) = active_hold() {
        if existing.display == display {
            let mut h = existing;
            h.deadline = deadline;
            h.heartbeat = now();
            h.requested = secs;
            write_hold(&h);
            report(secs, display);
            return;
        }
        kill_pid(existing.holder_pid); // display flag changed: holder must restart
        remove_hold_file();
    }
    let holder_pid = match spawn_holder(display) {
        Ok(pid) => pid,
        Err(e) => crate::common::fail(&e),
    };
    // give the holder a moment to take over and start heartbeating
    thread::sleep(Duration::from_millis(700));
    write_hold(&Hold { holder_pid, deadline, heartbeat: now(), display, release: false, requested: secs });
    report(secs, display);
    fn report(secs: u64, display: bool) {
        if secs >= FOREVER_SECS {
            println!("awake: holding until released{}", if display { " (display kept on too)" } else { "" });
        } else {
            println!("awake: holding for {}, sleep allowed again after that{}", fmt_remaining(secs), if display { " (display kept on too)" } else { "" });
        }
    }
}

fn release() {
    let Some(h) = read_hold() else {
        println!("awake: no hold active");
        return;
    };
    let mut h = h;
    h.release = true;
    h.heartbeat = now();
    write_hold(&h); // holder exits gracefully within one poll
    kill_pid(h.holder_pid); // belt and suspenders
    println!("awake: released");
}

pub fn run(args: &[String]) -> ! {
    // internal holder mode, spawned detached by `hold()`
    if args.first().map(|s| s.as_str()) == Some("--hold") {
        holder_loop(args.iter().any(|a| a == "--display"));
    }
    let display = args.iter().any(|a| a == "--display");
    let tokens: Vec<&str> = args.iter().map(|s| s.as_str()).filter(|a| *a != "--display").collect();
    match tokens.first().copied() {
        None => hold(FOREVER_SECS, display),
        Some("status") => status(),
        Some("off" | "release") => release(),
        Some(s) => match parse_duration(s) {
            Some(secs) if secs > 0 => hold(secs, display),
            _ => crate::common::fail(&format!("bad duration '{s}' - use e.g. 2h, 90m, 2h30m, 3600")),
        },
    }
    std::process::exit(0);
}

/// Suspend the machine now.
pub fn sleep_now() -> ! {
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("/usr/bin/pmset");
    #[cfg(windows)]
    let mut cmd = Command::new("rundll32.exe");
    #[cfg(target_os = "linux")]
    let mut cmd = Command::new("systemctl");
    #[cfg(target_os = "macos")]
    cmd.arg("sleepnow");
    #[cfg(windows)]
    cmd.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
    #[cfg(target_os = "linux")]
    cmd.arg("suspend");
    match cmd.status() {
        Ok(s) if s.success() => {
            println!("sleep: suspending now");
            std::process::exit(0)
        }
        Ok(s) => crate::common::fail(&format!("suspend request failed (exit {})", s.code().unwrap_or(-1))),
        Err(e) => crate::common::fail(&format!("cannot run suspend command: {e}")),
    }
}
