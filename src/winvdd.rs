// Windows virtual displays through the open-source Virtual Display Driver
// (github.com/VirtualDrivers/Virtual-Display-Driver, MIT; vendor/vdd).
//
// Windows can't add a screen without an indirect display driver. VDD is the
// one that is properly signed (SignPath Foundation, a GlobalSign chain
// Windows already trusts), so nothing gets added to the root store. The
// driver files are embedded in monctl.exe; `monctl driver install` (one UAC
// prompt) puts them in place - users never fetch or install a driver
// themselves.
//
// VDD is driven by a settings file (monitor count + allowed resolutions) and
// a named pipe; changing the count reloads the driver. Its settings live in
// monctl's own directory (VDDPATH), not shared with other VDD installs.

use crate::common::fail;
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::Duration;

const INF: &[u8] = include_bytes!("../vendor/vdd/MttVDD.inf");
const DLL: &[u8] = include_bytes!("../vendor/vdd/MttVDD.dll");
const CAT: &[u8] = include_bytes!("../vendor/vdd/mttvdd.cat");
const PUBLISHER_CERT: &[u8] = include_bytes!("../vendor/vdd/signpath-foundation.cer");
const HARDWARE_ID: &str = "Root\\MttVDD";
/// Adapter description VDD's monitors hang off (EnumDisplayDevices DeviceString).
pub const ADAPTER_NAME: &str = "Virtual Display Driver";
const PIPE: &str = "\\\\.\\pipe\\MTTVirtualDisplayPipe";

type HANDLE = *mut c_void;
type BOOL = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct GUID {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

// {4d36e968-e325-11ce-bfc1-08002be10318}
const DISPLAY_CLASS: GUID = GUID { d1: 0x4d36e968, d2: 0xe325, d3: 0x11ce, d4: [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18] };

#[repr(C)]
struct SP_DEVINFO_DATA {
    cb_size: u32,
    class_guid: GUID,
    dev_inst: u32,
    reserved: usize,
}

impl SP_DEVINFO_DATA {
    fn new() -> Self {
        SP_DEVINFO_DATA {
            cb_size: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            class_guid: DISPLAY_CLASS,
            dev_inst: 0,
            reserved: 0,
        }
    }
}

const DICD_GENERATE_ID: u32 = 0x1;
const SPDRP_HARDWAREID: u32 = 0x1;
const DIF_REGISTERDEVICE: u32 = 0x19;
const DIF_REMOVE: u32 = 0x05;
const INSTALLFLAG_FORCE: u32 = 0x1;
const INVALID_HANDLE: HANDLE = -1isize as HANDLE;

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiCreateDeviceInfoList(class: *const GUID, hwnd: HANDLE) -> HANDLE;
    fn SetupDiGetClassDevsW(class: *const GUID, enumerator: *const u16, hwnd: HANDLE, flags: u32) -> HANDLE;
    fn SetupDiEnumDeviceInfo(set: HANDLE, index: u32, data: *mut SP_DEVINFO_DATA) -> BOOL;
    fn SetupDiCreateDeviceInfoW(set: HANDLE, name: *const u16, class: *const GUID, desc: *const u16, hwnd: HANDLE, flags: u32, data: *mut SP_DEVINFO_DATA) -> BOOL;
    fn SetupDiSetDeviceRegistryPropertyW(set: HANDLE, data: *mut SP_DEVINFO_DATA, prop: u32, buf: *const u8, size: u32) -> BOOL;
    fn SetupDiGetDeviceRegistryPropertyW(set: HANDLE, data: *mut SP_DEVINFO_DATA, prop: u32, kind: *mut u32, buf: *mut u8, size: u32, needed: *mut u32) -> BOOL;
    fn SetupDiCallClassInstaller(dif: u32, set: HANDLE, data: *mut SP_DEVINFO_DATA) -> BOOL;
    fn SetupDiDestroyDeviceInfoList(set: HANDLE) -> BOOL;
}

#[link(name = "newdev")]
extern "system" {
    fn UpdateDriverForPlugAndPlayDevicesW(hwnd: HANDLE, hwid: *const u16, inf: *const u16, flags: u32, reboot: *mut BOOL) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *const c_void, disposition: u32, flags: u32, template: HANDLE) -> HANDLE;
    fn WriteFile(h: HANDLE, buf: *const u8, len: u32, written: *mut u32, overlapped: *mut c_void) -> BOOL;
    fn ReadFile(h: HANDLE, buf: *mut u8, len: u32, read: *mut u32, overlapped: *mut c_void) -> BOOL;
    fn CloseHandle(h: HANDLE) -> BOOL;
    fn WaitNamedPipeW(name: *const u16, timeout_ms: u32) -> BOOL;
    fn SetNamedPipeHandleState(h: HANDLE, mode: *const u32, max_collect: *const u32, timeout: *const u32) -> BOOL;
    fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegSetKeyValueW(key: isize, subkey: *const u16, value: *const u16, kind: u32, data: *const c_void, size: u32) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn IsUserAnAdmin() -> BOOL;
}

const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
const REG_SZ: u32 = 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Where the driver package and its settings live (machine-wide: the driver
/// runs as a service and reads it; users may write it, see install()).
pub fn home() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
    base.join("monctl").join("vdd")
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

// ---------------------------------------------------------- device node ----

fn for_each_vdd_device(mut f: impl FnMut(HANDLE, &mut SP_DEVINFO_DATA)) {
    unsafe {
        // all display-class devices, present or not
        let set = SetupDiGetClassDevsW(&DISPLAY_CLASS, std::ptr::null(), std::ptr::null_mut(), 0);
        if set == INVALID_HANDLE {
            return;
        }
        let mut i = 0;
        loop {
            let mut data = SP_DEVINFO_DATA::new();
            if SetupDiEnumDeviceInfo(set, i, &mut data) == 0 {
                break;
            }
            i += 1;
            let mut buf = [0u8; 1024];
            let mut kind = 0;
            if SetupDiGetDeviceRegistryPropertyW(set, &mut data, SPDRP_HARDWAREID, &mut kind, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut()) == 0 {
                continue;
            }
            let ids: Vec<u16> = buf.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let ids = String::from_utf16_lossy(&ids);
            if ids.split('\0').any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)) {
                f(set, &mut data);
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
}

/// Is the VDD device node there (installed by us or anyone)?
pub fn device_present() -> bool {
    let mut found = false;
    for_each_vdd_device(|_, _| found = true);
    found
}

fn create_device_node() -> Result<(), String> {
    unsafe {
        let set = SetupDiCreateDeviceInfoList(&DISPLAY_CLASS, std::ptr::null_mut());
        if set == INVALID_HANDLE {
            return Err(format!("SetupDiCreateDeviceInfoList failed ({})", GetLastError()));
        }
        let mut data = SP_DEVINFO_DATA::new();
        let class_name = wide("Display");
        let r = (|| {
            if SetupDiCreateDeviceInfoW(set, class_name.as_ptr(), &DISPLAY_CLASS, std::ptr::null(), std::ptr::null_mut(), DICD_GENERATE_ID, &mut data) == 0 {
                return Err(format!("SetupDiCreateDeviceInfo failed ({})", GetLastError()));
            }
            // REG_MULTI_SZ: the id, then an empty string
            let mut hwid: Vec<u16> = HARDWARE_ID.encode_utf16().collect();
            hwid.extend([0, 0]);
            let bytes: Vec<u8> = hwid.iter().flat_map(|w| w.to_le_bytes()).collect();
            if SetupDiSetDeviceRegistryPropertyW(set, &mut data, SPDRP_HARDWAREID, bytes.as_ptr(), bytes.len() as u32) == 0 {
                return Err(format!("setting the hardware id failed ({})", GetLastError()));
            }
            if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, &mut data) == 0 {
                return Err(format!("registering the device failed ({})", GetLastError()));
            }
            Ok(())
        })();
        SetupDiDestroyDeviceInfoList(set);
        r
    }
}

// --------------------------------------------------------------- install ----

/// Put the driver in place: files, settings, trusted publisher, device node,
/// driver binding, firewall rule for casting. Needs admin. Idempotent.
pub fn install() -> Result<(), String> {
    if !is_admin() {
        return Err("installing a driver needs administrator rights".into());
    }
    let dir = home();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, data) in [("MttVDD.inf", INF), ("MttVDD.dll", DLL), ("mttvdd.cat", CAT), ("signpath-foundation.cer", PUBLISHER_CERT)] {
        std::fs::write(dir.join(name), data).map_err(|e| format!("writing {name}: {e}"))?;
    }
    write_settings(&[], 0)?;
    // monctl runs as the user and rewrites the settings when tablets come
    // and go; the directory is created by an admin, so open it up
    run("icacls", &[&dir.to_string_lossy(), "/grant", "*S-1-5-32-545:(OI)(CI)M", "/T", "/Q"])?;
    unsafe {
        let key = wide("SOFTWARE\\MikeTheTech\\VirtualDisplayDriver");
        let name = wide("VDDPATH");
        let value = wide(&dir.to_string_lossy());
        let rc = RegSetKeyValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), name.as_ptr(), REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32);
        if rc != 0 {
            return Err(format!("writing the driver's settings path to the registry failed ({rc})"));
        }
    }
    // The package is signed by SignPath Foundation (publicly trusted chain);
    // trusting that publisher spares users Windows' "install this device
    // software?" prompt. TrustedPublisher only - nothing goes to the root store.
    run("certutil", &["-f", "-addstore", "TrustedPublisher", &dir.join("signpath-foundation.cer").to_string_lossy()])?;
    if !device_present() {
        create_device_node()?;
    }
    let inf = wide(&dir.join("MttVDD.inf").to_string_lossy());
    let hwid = wide(HARDWARE_ID);
    let mut reboot = 0;
    if unsafe { UpdateDriverForPlugAndPlayDevicesW(std::ptr::null_mut(), hwid.as_ptr(), inf.as_ptr(), INSTALLFLAG_FORCE, &mut reboot) } == 0 {
        return Err(format!("installing the driver failed ({})", unsafe { GetLastError() }));
    }
    // tablets reach the cast server over the LAN
    if let Ok(exe) = std::env::current_exe() {
        let program = format!("program={}", exe.display());
        let _ = run("netsh", &["advfirewall", "firewall", "delete", "rule", "name=monctl cast"]);
        run("netsh", &["advfirewall", "firewall", "add", "rule", "name=monctl cast", "dir=in", "action=allow", &program, "protocol=TCP", "profile=any", "enable=yes"])?;
    }
    Ok(())
}

/// Remove device node(s), the driver package and the firewall rule.
pub fn uninstall() -> Result<(), String> {
    if !is_admin() {
        return Err("removing a driver needs administrator rights".into());
    }
    for_each_vdd_device(|set, data| unsafe {
        SetupDiCallClassInstaller(DIF_REMOVE, set, data);
    });
    // driver store entry: pnputil names it oemNN.inf
    if let Ok(out) = std::process::Command::new("pnputil").arg("/enum-drivers").output() {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let mut published = None;
        for line in text.lines() {
            let v = line.split_once(':').map(|(_, v)| v.trim().to_string()).unwrap_or_default();
            if v.to_ascii_lowercase().starts_with("oem") && v.to_ascii_lowercase().ends_with(".inf") {
                published = Some(v);
            } else if v.eq_ignore_ascii_case("mttvdd.inf") {
                if let Some(p) = published.take() {
                    let _ = run("pnputil", &["/delete-driver", &p, "/uninstall", "/force"]);
                }
            }
        }
    }
    let _ = run("netsh", &["advfirewall", "firewall", "delete", "rule", "name=monctl cast"]);
    Ok(())
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stdout).trim()))
    }
}

/// Make sure the driver is there, asking for elevation once if it isn't:
/// re-runs `monctl driver install` elevated (the UAC prompt) and waits.
pub fn ensure() -> Result<(), String> {
    if device_present() && ping() {
        return Ok(());
    }
    if is_admin() {
        return install();
    }
    elevate(&["driver", "install"])?;
    // the driver service needs a moment after binding
    for _ in 0..50 {
        if ping() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("the virtual display driver did not start".into())
}

#[repr(C)]
struct SHELLEXECUTEINFOW {
    cb_size: u32,
    mask: u32,
    hwnd: HANDLE,
    verb: *const u16,
    file: *const u16,
    params: *const u16,
    dir: *const u16,
    show: i32,
    inst_app: HANDLE,
    id_list: *mut c_void,
    class: *const u16,
    hkey_class: HANDLE,
    hot_key: u32,
    icon: HANDLE,
    process: HANDLE,
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut SHELLEXECUTEINFOW) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn WaitForSingleObject(h: HANDLE, ms: u32) -> u32;
    fn GetExitCodeProcess(h: HANDLE, code: *mut u32) -> BOOL;
}

/// Run this exe elevated (UAC) with `args`, wait, return its exit status.
fn elevate(args: &[&str]) -> Result<(), String> {
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x40;
    const SW_HIDE: i32 = 0;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = wide(&exe.to_string_lossy());
    let params = wide(&args.join(" "));
    let verb = wide("runas");
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cb_size = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.mask = SEE_MASK_NOCLOSEPROCESS;
    info.verb = verb.as_ptr();
    info.file = file.as_ptr();
    info.params = params.as_ptr();
    info.show = SW_HIDE;
    unsafe {
        if ShellExecuteExW(&mut info) == 0 || info.process.is_null() {
            return Err("administrator permission was not granted (the virtual display driver needs it once)".into());
        }
        WaitForSingleObject(info.process, 0xFFFF_FFFF);
        let mut code = 1;
        GetExitCodeProcess(info.process, &mut code);
        CloseHandle(info.process);
        if code != 0 {
            return Err(format!("installing the virtual display driver failed (exit {code}; run `monctl driver install` as administrator to see why)"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------- pipe control ----

/// Send one command to the driver; returns its reply.
fn pipe(cmd: &str) -> Result<String, String> {
    const GENERIC_RW: u32 = 0x8000_0000 | 0x4000_0000;
    const OPEN_EXISTING: u32 = 3;
    const PIPE_READMODE_MESSAGE: u32 = 2;
    let name = wide(PIPE);
    unsafe {
        if WaitNamedPipeW(name.as_ptr(), 2000) == 0 {
            return Err("the virtual display driver is not running".into());
        }
        let h = CreateFileW(name.as_ptr(), GENERIC_RW, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
        if h == INVALID_HANDLE {
            return Err(format!("cannot talk to the virtual display driver ({})", GetLastError()));
        }
        let mode = PIPE_READMODE_MESSAGE;
        SetNamedPipeHandleState(h, &mode, std::ptr::null(), std::ptr::null());
        // the driver compares wide strings (wcsncmp)
        let msg: Vec<u8> = cmd.encode_utf16().flat_map(|w| w.to_le_bytes()).collect();
        let mut n = 0;
        let ok = WriteFile(h, msg.as_ptr(), msg.len() as u32, &mut n, std::ptr::null_mut()) != 0;
        let mut reply = Vec::new();
        if ok {
            let mut buf = [0u8; 4096];
            let mut got = 0;
            if ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut got, std::ptr::null_mut()) != 0 {
                reply.extend_from_slice(&buf[..got as usize]);
            }
        }
        CloseHandle(h);
        if !ok {
            return Err("writing to the virtual display driver failed".into());
        }
        let words: Vec<u16> = reply.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Ok(String::from_utf16_lossy(&words).trim_end_matches('\0').to_string())
    }
}

pub fn ping() -> bool {
    pipe("PING").is_ok()
}

/// Write the settings: `count` monitors, offering `sizes` (plus a few common
/// ones so a monitor is usable before it is resized) at 60 Hz.
fn write_settings(sizes: &[(u32, u32)], count: usize) -> Result<(), String> {
    let mut all: Vec<(u32, u32)> = sizes.to_vec();
    all.extend([(1920, 1080), (2560, 1440), (1280, 800), (2048, 1536)]);
    all.dedup();
    let mut seen = vec![];
    let mut res = String::new();
    for (w, h) in all {
        if seen.contains(&(w, h)) {
            continue;
        }
        seen.push((w, h));
        res.push_str(&format!("        <resolution><width>{w}</width><height>{h}</height><refresh_rate>60</refresh_rate></resolution>\n"));
    }
    let xml = format!(
        "<?xml version='1.0' encoding='utf-8'?>\n<!-- written by monctl: tablet screens come and go with the cast server -->\n<vdd_settings>\n    <monitors><count>{count}</count></monitors>\n    <gpu><friendlyname>default</friendlyname></gpu>\n    <global><g_refresh_rate>60</g_refresh_rate></global>\n    <resolutions>\n{res}    </resolutions>\n    <options>\n        <CustomEdid>false</CustomEdid>\n        <PreventSpoof>false</PreventSpoof>\n        <EdidCeaOverride>false</EdidCeaOverride>\n        <HardwareCursor>false</HardwareCursor>\n        <SDR10bit>false</SDR10bit>\n        <HDRPlus>false</HDRPlus>\n        <logging>false</logging>\n        <debuglogging>false</debuglogging>\n    </options>\n</vdd_settings>\n"
    );
    let path = home().join("vdd_settings.xml");
    std::fs::write(&path, xml).map_err(|e| format!("{}: {e}", path.display()))
}

/// `\\.\DISPLAYn` names of the driver's monitors that are on the desktop.
pub fn displays() -> Vec<String> {
    crate::windows::adapters_named(ADAPTER_NAME)
}

/// Show exactly `sizes.len()` virtual monitors offering those sizes. The
/// driver reloads (every VDD monitor is recreated); returns the new monitor
/// names, in creation order.
pub fn set_monitors(sizes: &[(u32, u32)]) -> Result<Vec<String>, String> {
    write_settings(sizes, sizes.len())?;
    pipe(&format!("SETDISPLAYCOUNT {}", sizes.len()))?;
    // wait for the reload to settle on the new count
    let mut last = vec![];
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(200));
        let now = displays();
        if now.len() == sizes.len() && now == last {
            return Ok(now);
        }
        last = now;
    }
    if sizes.is_empty() {
        return Ok(vec![]);
    }
    Err(format!("expected {} virtual display(s), the driver shows {}", sizes.len(), last.len()))
}

// ----------------------------------------------------------------- CLI ----

pub fn run_cli(args: &[String]) -> ! {
    match args.first().map(|s| s.as_str()) {
        Some("install") => match install() {
            Ok(()) => println!("virtual display driver installed ({})", home().display()),
            Err(e) => fail(&e),
        },
        Some("uninstall" | "remove") => match uninstall() {
            Ok(()) => println!("virtual display driver removed"),
            Err(e) => fail(&e),
        },
        Some("status") | None => {
            let present = device_present();
            let running = present && ping();
            println!(
                "virtual display driver: {}",
                match (present, running) {
                    (false, _) => "not installed (monctl installs it the first time a tablet connects, or: monctl driver install)",
                    (true, false) => "installed, not running",
                    (true, true) => "installed and running",
                }
            );
            for d in displays() {
                println!("  {d}");
            }
        }
        Some("count") => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| fail("usage: monctl driver count N"));
            let sizes = vec![(1920, 1080); n];
            match ensure().and_then(|()| set_monitors(&sizes)) {
                Ok(d) => println!("{} virtual display(s): {}", d.len(), d.join(" ")),
                Err(e) => fail(&e),
            }
        }
        _ => fail("usage: monctl driver [status|install|uninstall|count N]"),
    }
    std::process::exit(0)
}
