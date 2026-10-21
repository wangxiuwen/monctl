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
// VDD is configured by a settings file (monitor count + offered
// resolutions) read when the device starts. Its runtime "reload" is not
// safe to use (SETDISPLAYCOUNT never answers on the pipe and its reload
// path crashes the device when a monitor is detached - problem code 43), so
// monctl never reloads: the device starts with a fixed pool of monitors
// offering the usual tablet resolutions, all kept off the desktop ("parked")
// until a tablet claims one. Settings live in monctl's own directory
// (VDDPATH), not shared with other VDD installs.

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
/// Monitors the device starts with: one per concurrently connected tablet.
pub const POOL: usize = 4;
/// The driver builds no monitor at all when offered more than 50 modes
/// (51 fails, verified on 26200) - so this list is curated, not exhaustive.
const MAX_MODES: usize = 50;
/// Physical resolutions of common tablets (landscape; portrait is added too,
/// so each costs two modes). Tablets not listed get the closest mode of the
/// same aspect ratio and are learned (`learn_size`) for the next start.
const TABLET_SIZES: &[(u32, u32)] = &[
    // iPad: 9th gen, Air / 10th, Pro 11, Pro 11 (M4), Pro 12.9, Pro 13 (M4), mini 6, older 9.7"
    (2160, 1620), (2360, 1640), (2388, 1668), (2420, 1668), (2732, 2048), (2752, 2064), (2266, 1488), (2048, 1536),
    // Android: Samsung Tab S (11" / + / Ultra), Xiaomi Pad, the common 16:10 panels
    (2560, 1600), (2800, 1752), (2960, 1848), (2880, 1800), (1920, 1200), (2000, 1200), (1280, 800),
    // Surface Pro 8-11, Pro 4-7, Go
    (2880, 1920), (2736, 1824), (1920, 1280),
    // plain 1080p
    (1920, 1080),
];

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
    fn SetupDiSetClassInstallParamsW(set: HANDLE, data: *mut SP_DEVINFO_DATA, params: *const c_void, size: u32) -> BOOL;
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

/// Restart the device (after the driver crashed: problem code 43). Admin.
pub fn restart() -> Result<(), String> {
    #[repr(C)]
    struct PropChangeParams {
        cb_size: u32,
        install_function: u32,
        state_change: u32,
        scope: u32,
        hw_profile: u32,
    }
    const DIF_PROPERTYCHANGE: u32 = 0x12;
    const DICS_PROPCHANGE: u32 = 3;
    const DICS_FLAG_GLOBAL: u32 = 1;
    if !is_admin() {
        return Err("restarting a driver needs administrator rights".into());
    }
    let mut result = Err("the virtual display driver is not installed".to_string());
    for_each_vdd_device(|set, data| unsafe {
        let params = PropChangeParams { cb_size: 8, install_function: DIF_PROPERTYCHANGE, state_change: DICS_PROPCHANGE, scope: DICS_FLAG_GLOBAL, hw_profile: 0 };
        let ok = SetupDiSetClassInstallParamsW(set, data, &params as *const PropChangeParams as *const c_void, std::mem::size_of::<PropChangeParams>() as u32) != 0
            && SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, data) != 0;
        result = if ok { Ok(()) } else { Err(format!("restarting the driver failed ({})", GetLastError())) };
    });
    if result.is_ok() {
        for _ in 0..25 {
            if pool_ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        park();
    }
    result
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
    write_settings()?;
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
    // it comes up with its monitor pool on the desktop: park them
    for _ in 0..25 {
        if crate::windows::adapters_named(ADAPTER_NAME, false).len() >= POOL {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    park();
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
    if device_present() && ping() && pool_ready() {
        return Ok(());
    }
    // installed but stopped (crashed: Windows parks it at problem 43), or
    // started with older settings (no pool yet): restart with ours
    let action = if device_present() { "restart" } else { "install" };
    if action == "restart" {
        write_settings()?;
    }
    if is_admin() {
        if action == "restart" {
            restart()?;
        } else {
            install()?;
        }
    } else {
        elevate(&["driver", action])?;
    }
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

/// Send one command to the driver; returns its reply. Bounded: some commands
/// never answer and a wedged driver must not hang the caller.
fn pipe(cmd: &str) -> Result<String, String> {
    let cmd = cmd.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(pipe_blocking(&cmd));
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap_or_else(|_| Err("the virtual display driver does not respond".into()))
}

fn pipe_blocking(cmd: &str) -> Result<String, String> {
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

/// Sizes seen from tablets that weren't in TABLET_SIZES: offered from the
/// next device start on (reboot / reinstall), so that tablet gets its exact
/// size; until then it gets the closest one.
fn learned_path() -> PathBuf {
    home().join("learned_sizes.txt")
}

fn learned_sizes() -> Vec<(u32, u32)> {
    std::fs::read_to_string(learned_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('x').and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?))))
        .collect()
}

pub fn learn_size(w: u32, h: u32) {
    let mut known = learned_sizes();
    let (w, h) = (w.max(h), w.min(h)); // stored landscape; both orientations are offered
    if known.contains(&(w, h)) || TABLET_SIZES.contains(&(w, h)) {
        return;
    }
    known.push((w, h));
    // stay under the driver's mode limit: the oldest learned sizes go first
    let room = (MAX_MODES - TABLET_SIZES.len() * 2) / 2;
    while known.len() > room {
        known.remove(0);
    }
    let text: String = known.iter().map(|(w, h)| format!("{w}x{h}\n")).collect();
    let _ = std::fs::write(learned_path(), text);
    let _ = write_settings();
}

/// The settings the device reads when it starts: the monitor pool, every
/// tablet size in both orientations, 60 Hz.
fn write_settings() -> Result<(), String> {
    let mut sizes: Vec<(u32, u32)> = vec![];
    for &(w, h) in TABLET_SIZES.iter().chain(learned_sizes().iter()) {
        for s in [(w, h), (h, w)] {
            if !sizes.contains(&s) {
                sizes.push(s);
            }
        }
    }
    let res: String = sizes
        .iter()
        .map(|(w, h)| format!("        <resolution><width>{w}</width><height>{h}</height><refresh_rate>60</refresh_rate></resolution>\n"))
        .collect();
    let xml = format!(
        "<?xml version='1.0' encoding='utf-8'?>\n<!-- written by monctl: a pool of tablet screens, parked until a tablet claims one -->\n<vdd_settings>\n    <monitors><count>{POOL}</count></monitors>\n    <gpu><friendlyname>default</friendlyname></gpu>\n    <global><g_refresh_rate>60</g_refresh_rate></global>\n    <resolutions>\n{res}    </resolutions>\n    <options>\n        <CustomEdid>false</CustomEdid>\n        <PreventSpoof>false</PreventSpoof>\n        <EdidCeaOverride>false</EdidCeaOverride>\n        <HardwareCursor>false</HardwareCursor>\n        <SDR10bit>false</SDR10bit>\n        <HDRPlus>false</HDRPlus>\n        <logging>false</logging>\n        <debuglogging>false</debuglogging>\n    </options>\n</vdd_settings>\n"
    );
    let path = home().join("vdd_settings.xml");
    std::fs::write(&path, xml).map_err(|e| format!("{}: {e}", path.display()))
}

/// `\\.\DISPLAYn` names of the driver's monitors that are on the desktop.
pub fn displays() -> Vec<String> {
    crate::windows::adapters_named(ADAPTER_NAME, true)
}

/// Keep every monitor of the pool off the desktop (like `monctl off` does
/// for a real screen) so windows never wander onto a screen nobody sees.
/// Needs no admin rights.
pub fn park() {
    for d in displays() {
        let _ = crate::windows::set_enabled(&d, false);
    }
}

/// All monitors of the pool, parked or not, in a stable order.
pub fn pool() -> Vec<String> {
    crate::windows::adapters_named(ADAPTER_NAME, false)
}

/// Put pool monitor `name` on the desktop at `w`x`h` (or the closest offered
/// size of the same orientation). Returns the size it got.
pub fn claim(name: &str, w: u32, h: u32) -> Result<(u32, u32), String> {
    if !displays().iter().any(|d| d == name) {
        crate::windows::set_enabled(name, true)?;
    }
    let modes = crate::windows::modes(name)?;
    let portrait = h > w;
    let best = modes
        .iter()
        .filter(|m| m.hz == 0 || m.hz >= 59)
        .filter(|m| (m.height > m.width) == portrait)
        .min_by_key(|m| {
            // exact first, then the same aspect ratio, then the closest area
            let aspect = ((m.width as f64 / m.height as f64) - (w as f64 / h as f64)).abs();
            ((aspect * 1000.0) as i64, ((m.width * m.height) as i64 - (w * h) as i64).abs())
        })
        .cloned()
        .ok_or_else(|| format!("{name} offers no usable mode"))?;
    if (best.width, best.height) != (w, h) {
        learn_size(w, h);
    }
    if !best.current {
        crate::windows::set_mode(name, &best)?;
    }
    Ok((best.width, best.height))
}

/// Is the running device configured as monctl's pool? (An older install, or
/// one whose settings changed, needs a device restart to pick them up.)
pub fn pool_ready() -> bool {
    pool().len() >= POOL
}

// ----------------------------------------------------------------- CLI ----

pub fn run_cli(args: &[String]) -> ! {
    match args.first().map(|s| s.as_str()) {
        Some("install") => match install() {
            Ok(()) => println!("virtual display driver installed ({})", home().display()),
            Err(e) => fail(&e),
        },
        Some("restart") => match restart() {
            Ok(()) => println!("virtual display driver restarted"),
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
            let on = displays();
            for d in pool() {
                println!("  {d}  {}", if on.contains(&d) { "on the desktop" } else { "parked" });
            }
        }
        Some("park") => {
            park();
            println!("virtual display parked");
        }
        Some("claim") => {
            let (w, h) = args.get(1).and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((1920, 1080));
            let Some(name) = pool().into_iter().next() else { fail("the virtual display driver has no monitors (monctl driver restart)") };
            match claim(&name, w, h) {
                Ok((gw, gh)) => println!("{name} on the desktop at {gw}x{gh} (monctl driver park removes it)"),
                Err(e) => fail(&e),
            }
        }
        _ => fail("usage: monctl driver [status|install|uninstall|restart|claim WxH|park]"),
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tablet_sizes_fit_the_driver_mode_limit() {
        let modes = super::TABLET_SIZES.len() * 2;
        assert!(modes <= super::MAX_MODES - 8, "{modes} modes leave no room to learn sizes");
        let mut seen = std::collections::BTreeSet::new();
        for &(w, h) in super::TABLET_SIZES {
            assert!(w > h && w % 2 == 0 && h % 2 == 0, "{w}x{h}: landscape, even sizes only");
            assert!(seen.insert((w, h)), "{w}x{h} listed twice");
        }
    }
}
