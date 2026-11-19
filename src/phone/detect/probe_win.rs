// Windows probes behind the detectors: processes, their windows, and who
// is connected. All plain Win32, zero deps.

use std::ffi::c_void;

type HWND = *mut c_void;
type LPARAM = isize;

#[link(name = "kernel32")]
extern "system" {
    fn K32EnumProcesses(pids: *mut u32, bytes: u32, used: *mut u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
    fn QueryFullProcessImageNameW(h: *mut c_void, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
}

#[link(name = "iphlpapi")]
extern "system" {
    fn GetExtendedTcpTable(table: *mut c_void, size: *mut u32, order: i32, family: u32, class: i32, zero: u32) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: Option<unsafe extern "system" fn(HWND, LPARAM) -> i32>, lparam: LPARAM) -> i32;
    fn IsWindowVisible(h: HWND) -> i32;
    fn GetWindowTextLengthW(h: HWND) -> i32;
    fn GetWindowTextW(h: HWND, buf: *mut u16, max: i32) -> i32;
    fn GetWindowThreadProcessId(h: HWND, pid: *mut u32) -> u32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TCP_TABLE_OWNER_PID_ALL: i32 = 5;
const AF_INET: u32 = 2;

/// Every running process name, lowercased without the .exe suffix.
pub fn processes() -> Vec<String> {
    unsafe {
        let mut pids = [0u32; 4096];
        let mut used = 0u32;
        if K32EnumProcesses(pids.as_mut_ptr(), std::mem::size_of_val(&pids) as u32, &mut used) == 0 {
            return vec![];
        }
        let n = (used as usize) / std::mem::size_of::<u32>();
        let mut out = Vec::new();
        for &pid in &pids[..n] {
            if pid == 0 {
                continue;
            }
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                continue;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok != 0 {
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                if let Some(name) = path.rsplit(['\\', '/']).next() {
                    out.push(name.trim_end_matches(".exe").to_ascii_lowercase());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

pub fn process_running(names: &[&str]) -> bool {
    let running = processes();
    names.iter().any(|n| running.contains(&n.to_ascii_lowercase()))
}

unsafe extern "system" fn window_cb(h: HWND, lparam: LPARAM) -> i32 {
    let ctx = &mut *(lparam as *mut (Vec<(u32, String)>, ()));
    if IsWindowVisible(h) == 0 {
        return 1;
    }
    let len = GetWindowTextLengthW(h);
    if len <= 0 {
        return 1;
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, buf.as_mut_ptr(), len as i32 + 1);
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    ctx.0.push((pid, String::from_utf16_lossy(&buf[..n as usize])));
    1
}

/// Whether a process whose image matches `process` (substring, lowercase)
/// shows a visible titled window whose title contains `title_substr`.
pub fn window_present(process: &str, title_substr: &str) -> bool {
    unsafe {
        let mut wins: Vec<(u32, String)> = Vec::new();
        EnumWindows(Some(window_cb), &mut wins as *mut _ as LPARAM);
        wins.iter().any(|(pid, title)| {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, *pid);
            if h.is_null() {
                return false;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            let img = if ok == 0 { String::new() } else { String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase() };
            img.contains(process) && title.to_lowercase().contains(title_substr)
        })
    }
}

/// Whether a process owns any visible titled window at all.
pub fn any_window(process: &str) -> bool {
    unsafe {
        let mut wins: Vec<(u32, String)> = Vec::new();
        EnumWindows(Some(window_cb), &mut wins as *mut _ as LPARAM);
        wins.iter().any(|(pid, _)| {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, *pid);
            if h.is_null() {
                return false;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            ok != 0 && {
                let img = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
                img.contains(process)
            }
        })
    }
}

/// A process with an established outbound IPv4 TCP connection (the relay
/// peer a controller talks through).
pub fn tcp_established(process: &str) -> bool {
        #[repr(C)]
    #[derive(Clone, Copy)]
    struct MibRow {
        state: u32,
        local: u32,
        local_port: u32,
        remote: u32,
        remote_port: u32,
        pid: u32,
    }
    unsafe {
        let mut size = 0u32;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 {
            return false;
        }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedTcpTable(buf.as_mut_ptr() as *mut c_void, &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0) != 0 {
            return false;
        }
        let count = *(buf.as_ptr() as *const u32);
        let rows = buf.as_ptr().add(4) as *const MibRow;
        let pids = pids_of(process);
        (0..count as usize).any(|i| {
            let r = *rows.add(i);
            r.state == 5 /* ESTABLISHED */ && pids.contains(&r.pid)
        })
    }
}

fn pids_of(process: &str) -> Vec<u32> {
    unsafe {
        let mut pids = [0u32; 4096];
        let mut used = 0u32;
        if K32EnumProcesses(pids.as_mut_ptr(), std::mem::size_of_val(&pids) as u32, &mut used) == 0 {
            return vec![];
        }
        let n = (used as usize) / std::mem::size_of::<u32>();
        let mut out = Vec::new();
        for &pid in &pids[..n] {
            if pid == 0 {
                continue;
            }
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                continue;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                continue;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
            if let Some(name) = path.rsplit(['\\', '/']).next() {
                if name.contains(process) {
                    out.push(pid);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_enumerate_without_false_hits() {
        let ps = processes();
        assert!(!ps.is_empty());
        // Something boring is always running on Windows.
        assert!(process_running(&["svchost", "explorer"]), "no svchost/explorer in {:?}", &ps[..ps.len().min(20)]);
        assert!(!process_running(&["definitely-not-a-process-xyz"]));
    }

    #[test]
    fn window_probe_reports_absence_cleanly() {
        assert!(!window_present("definitely-not-a-process", "zzz"));
        assert!(!any_window("definitely-not-a-process"));
        assert!(!tcp_established("definitely-not-a-process"));
    }
}

/// Whether a file whose name starts with `prefix` was written within
/// `max_age_secs` in `dir`. This is how UU's controlled session is
/// detected: GameViewerServer writes `streamer_log_controlled_<ts>_<pid>.slog`
/// only while a controller is actually streaming this machine, refreshed
/// as the session runs - a calibrated signal, not a guess.
pub fn fresh_file(dir: &std::path::Path, prefix: &str, max_age_secs: u64) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(prefix) {
            continue;
        }
        let fresh = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| now.duration_since(m).ok()).map(|d| d.as_secs() <= max_age_secs);
        if fresh == Some(true) {
            return true;
        }
    }
    false
}

/// GameViewerServer's log directory: the running process's install root,
/// else the default install path.
pub fn uu_server_log_dir() -> Option<std::path::PathBuf> {
    // The image path comes from pids_of's enumeration; reconstruct it from
    // the same query rather than a second pass.
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    unsafe {
        let mut pids = [0u32; 4096];
        let mut used = 0u32;
        if K32EnumProcesses(pids.as_mut_ptr(), std::mem::size_of_val(&pids) as u32, &mut used) == 0 {
            return None;
        }
        let n = (used as usize) / std::mem::size_of::<u32>();
        for &pid in &pids[..n] {
            if pid == 0 {
                continue;
            }
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                continue;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                continue;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
            if path.contains("gameviewerserver") {
                if let Some(root) = std::path::Path::new(&path).ancestors().nth(2) {
                    roots.push(root.to_path_buf());
                }
            }
        }
    }
    let root = roots
        .into_iter()
        .next()
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Program Files\Netease\GameViewer"));
    Some(root.join("log").join("server").join("log"))
}

/// The calibrated UU "this machine is being controlled" signal. The streamer
/// log is event-driven: ~5-15s apart while the picture is interactive, and
/// it can pause 30s+ when the phone just watches a still screen - so the
/// freshness window is 60s, not tighter.
pub fn uu_controlled() -> bool {
    let Some(dir) = uu_server_log_dir() else { return false };
    fresh_file(&dir, "streamer_log_controlled_", 60)
}
