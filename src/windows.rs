// Windows backend — ChangeDisplaySettingsEx-based monitor detach/attach, the same
// approach NirSoft MultiMonitorTool uses. No external dependencies; raw user32 FFI.
//
// Caveat: detach behavior is driver-dependent (works on the vast majority of
// WDDM drivers); a disabled display keeps its registry config, so re-enabling
// places it to the right of the current desktop.

use crate::common::DisplayInfo;
use std::ffi::c_void;

type DWORD = u32;
type LONG = i32;
type BOOL = i32;
type WCHAR = u16;

const ENUM_CURRENT_SETTINGS: DWORD = 0xFFFF_FFFF;
const EDD_GET_DEVICE_INTERFACE_NAME: DWORD = 0x0000_0001;
const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: DWORD = 0x0000_0001;
const DISPLAY_DEVICE_PRIMARY_DEVICE: DWORD = 0x0000_0004;
const DISPLAY_DEVICE_MIRRORING_DRIVER: DWORD = 0x0000_0008;
const DM_POSITION: DWORD = 0x0000_0020;
const DM_PELSWIDTH: DWORD = 0x0008_0000;
const DM_PELSHEIGHT: DWORD = 0x0010_0000;
const DM_DISPLAYFREQUENCY: DWORD = 0x0040_0000;
const CDS_UPDATEREGISTRY: DWORD = 0x0000_0001;
const CDS_NORESET: DWORD = 0x1000_0000;
const DISP_CHANGE_SUCCESSFUL: LONG = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct POINTL {
    x: LONG,
    y: LONG,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DevModePrinter {
    dm_orientation: i16,
    dm_paper_size: i16,
    dm_paper_length: i16,
    dm_paper_width: i16,
    dm_scale: i16,
    dm_copies: i16,
    dm_default_source: i16,
    dm_print_quality: i16,
}

// DEVMODEW must match the winuser.h layout byte for byte: dmPosition /
// dmDisplayOrientation / dmDisplayFixedOutput are alternatives in the SAME
// union as the printer fields, not separate fields after it. Getting this
// wrong shifts every later member and makes ChangeDisplaySettingsExW fail
// with DISP_CHANGE_BADMODE on real drivers.
#[repr(C)]
union DevModeU {
    printer: DevModePrinter,
    position: POINTL,
    display_orientation: DWORD,
    display_fixed_output: DWORD,
}

#[repr(C)]
union DevModeU2 {
    dm_display_flags: DWORD,
    dm_nup: DWORD,
}

#[repr(C)]
struct DEVMODEW {
    dm_device_name: [WCHAR; 32],
    dm_spec_version: u16,
    dm_driver_version: u16,
    dm_size: u16,
    dm_driver_extra: u16,
    dm_fields: DWORD,
    u: DevModeU,
    dm_color: i16,
    dm_duplex: i16,
    dm_y_resolution: i16,
    dm_tt_option: i16,
    dm_collate: i16,
    dm_form_name: [WCHAR; 32],
    dm_log_pixels: u16,
    dm_bits_per_pel: DWORD,
    dm_pels_width: DWORD,
    dm_pels_height: DWORD,
    u2: DevModeU2,
    dm_display_frequency: DWORD,
    dm_icm_method: DWORD,
    dm_icm_intent: DWORD,
    dm_media_type: DWORD,
    dm_dither_type: DWORD,
    dm_reserved1: DWORD,
    dm_reserved2: DWORD,
    dm_panning_width: DWORD,
    dm_panning_height: DWORD,
}

impl DEVMODEW {
    fn new() -> Self {
        // SAFETY: DEVMODEW is a plain-old-data struct; zeroed is the API-required init.
        let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
        dm.dm_size = std::mem::size_of::<DEVMODEW>() as u16;
        dm
    }
}

const _: () = assert!(std::mem::size_of::<DEVMODEW>() == 220); // x64 SDK layout

#[repr(C)]
struct DISPLAY_DEVICEW {
    cb: DWORD,
    device_name: [WCHAR; 32],
    device_string: [WCHAR; 128],
    state_flags: DWORD,
    device_id: [WCHAR; 128],
    device_key: [WCHAR; 128],
}

impl DISPLAY_DEVICEW {
    fn new() -> Self {
        let mut dd: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as DWORD;
        dd
    }
}

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayDevicesW(device: *const WCHAR, idx: DWORD, out: *mut DISPLAY_DEVICEW, flags: DWORD) -> BOOL;
    fn EnumDisplaySettingsW(device: *const WCHAR, mode: DWORD, out: *mut DEVMODEW) -> BOOL;
    fn ChangeDisplaySettingsExW(
        device: *const WCHAR,
        mode: *mut DEVMODEW,
        hwnd: *mut c_void,
        flags: DWORD,
        param: *mut c_void,
    ) -> LONG;
}

fn wide_to_string(w: &[WCHAR]) -> String {
    let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..len])
}

fn string_to_wide(s: &str) -> Vec<WCHAR> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Enumerate display adapters (\\.\DISPLAYn). A display disabled by monctl keeps
/// showing up here but without the ACTIVE flag.
fn adapters() -> Vec<(String, String, DWORD)> {
    let mut out = vec![];
    for idx in 0..64u32 {
        let mut dd = DISPLAY_DEVICEW::new();
        if unsafe { EnumDisplayDevicesW(std::ptr::null(), idx, &mut dd, 0) } == 0 {
            break;
        }
        let flags = dd.state_flags;
        if flags & DISPLAY_DEVICE_MIRRORING_DRIVER != 0 {
            continue; // mirror driver / pseudo device
        }
        out.push((wide_to_string(&dd.device_name), wide_to_string(&dd.device_string), flags));
    }
    out
}

fn current_settings(device: &str) -> Option<DEVMODEW> {
    let wide = string_to_wide(device);
    let mut dm = DEVMODEW::new();
    if unsafe { EnumDisplaySettingsW(wide.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) } != 0 {
        Some(dm)
    } else {
        None
    }
}

/// Best registry mode with real dimensions — fallback for devices whose
/// "current settings" are zeroed (detached or phantom outputs), so a re-attach
/// can never apply a 0x0 mode and kill a screen for good. Among modes with the
/// largest area, prefer the highest refresh rate — mode 0 is often 50 Hz, and
/// silently dropping a 60 Hz panel to 50 Hz on re-attach is a bad surprise.
fn any_real_mode(device: &str) -> Option<DEVMODEW> {
    let wide = string_to_wide(device);
    let mut best: Option<DEVMODEW> = None;
    for i in 0..200u32 {
        let mut dm = DEVMODEW::new();
        if unsafe { EnumDisplaySettingsW(wide.as_ptr(), i, &mut dm) } == 0 {
            break;
        }
        if dm.dm_pels_width == 0 || dm.dm_pels_height == 0 {
            continue;
        }
        best = Some(match best {
            None => dm,
            Some(b) => {
                let area = dm.dm_pels_width as u64 * dm.dm_pels_height as u64;
                let best_area = b.dm_pels_width as u64 * b.dm_pels_height as u64;
                if area > best_area || (area == best_area && dm.dm_display_frequency > b.dm_display_frequency) {
                    dm
                } else {
                    b
                }
            }
        });
    }
    best
}

/// EDID monitor name attached to an adapter ("DELL P2314H"), falling back to
/// the adapter string ("NVIDIA GeForce RTX 4070") when no monitor is attached.
fn monitor_name(adapter: &str) -> Option<String> {
    let wide = string_to_wide(adapter);
    let mut dd = DISPLAY_DEVICEW::new();
    if unsafe { EnumDisplayDevicesW(wide.as_ptr(), 0, &mut dd, EDD_GET_DEVICE_INTERFACE_NAME) } != 0 {
        let s = wide_to_string(&dd.device_string);
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

pub fn list() -> Vec<DisplayInfo> {
    adapters()
        .into_iter()
        .map(|(device, description, flags)| {
            // 0x10 is DISPLAY_DEVICE_VGA_COMPATIBLE, not "active": OR-ing it in made
            // detached VGA-compatible adapters look active forever
            let active = flags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0;
            let main = flags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0;
            let detail = current_settings(&device)
                .filter(|_| active)
                .map(|dm| {
                    if dm.dm_pels_width == 0 || dm.dm_pels_height == 0 {
                        return "n/a".to_string();
                    }
                    let hz = dm.dm_display_frequency;
                    let (x, y) = unsafe { (dm.u.position.x, dm.u.position.y) };
                    let r = if hz > 1 { format!(" @{}Hz", hz) } else { String::new() };
                    format!("{}x{}{} ({},{})", dm.dm_pels_width, dm.dm_pels_height, r, x, y)
                })
                .unwrap_or_else(|| "n/a".into());
            let name = monitor_name(&device).unwrap_or(description);
            DisplayInfo {
                id: device,
                active,
                main,
                name: if name.is_empty() { "Display".into() } else { name },
                detail,
            }
        })
        .collect()
}

/// Apply a pending per-device change, then flush the whole desktop config.
fn apply(device: Option<&str>, dm: &mut DEVMODEW) -> Result<(), String> {
    let wide = device.map(string_to_wide);
    let dev_ptr = match &wide {
        Some(w) => w.as_ptr(),
        None => std::ptr::null(),
    };
    let rc = unsafe { ChangeDisplaySettingsExW(dev_ptr, dm, std::ptr::null_mut(), CDS_UPDATEREGISTRY | CDS_NORESET, std::ptr::null_mut()) };
    if rc != DISP_CHANGE_SUCCESSFUL {
        return Err(format!("ChangeDisplaySettingsExW returned {rc}"));
    }
    let rc = unsafe { ChangeDisplaySettingsExW(std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), 0, std::ptr::null_mut()) };
    if rc != DISP_CHANGE_SUCCESSFUL {
        return Err(format!("apply (ChangeDisplaySettingsExW) returned {rc}"));
    }
    Ok(())
}

/// Whether this device can be re-attached at all: it has a usable current or
/// stored mode. Phantom outputs (GPU heads / virtual adapters with no panel)
/// have none and would fail attach with "no usable stored mode" — `monctl all`
/// skips those quietly instead of erroring on each.
pub fn can_attach(id: &str) -> bool {
    current_settings(id)
        .filter(|dm| dm.dm_pels_width > 0 && dm.dm_pels_height > 0)
        .is_some()
        || any_real_mode(id).is_some()
}

/// Highest refresh rate the device advertises at exactly this resolution.
/// Detaching can leave the driver's "current" mode at a low-refresh entry
/// (e.g. 1920x1080@50 on a 60 Hz panel); re-attach should never downgrade.
fn best_frequency_at(device: &str, width: DWORD, height: DWORD) -> Option<DWORD> {
    let wide = string_to_wide(device);
    let mut best = None;
    for i in 0..200u32 {
        let mut dm = DEVMODEW::new();
        if unsafe { EnumDisplaySettingsW(wide.as_ptr(), i, &mut dm) } == 0 {
            break;
        }
        if dm.dm_pels_width == width && dm.dm_pels_height == height {
            best = Some(best.map_or(dm.dm_display_frequency, |b: DWORD| b.max(dm.dm_display_frequency)));
        }
    }
    best
}

pub fn set_enabled(id: &str, enable: bool) -> Result<(), String> {
    if !id.starts_with("\\\\.\\DISPLAY") {
        return Err(format!("'{id}' is not a valid Windows display id (expected \\\\.\\DISPLAYn, see `monctl list`)"));
    }
    if enable {
        // attach: restore the stored mode (or any real mode), placed to the right of the desktop
        let mut dm = match current_settings(id).filter(|dm| dm.dm_pels_width > 0 && dm.dm_pels_height > 0) {
            Some(dm) => dm,
            None => any_real_mode(id).ok_or_else(|| format!("no usable stored mode for {id} — cannot re-attach it"))?,
        };
        // never re-attach below what the panel advertises at this resolution
        if let Some(hz) = best_frequency_at(id, dm.dm_pels_width, dm.dm_pels_height) {
            if hz > dm.dm_display_frequency {
                dm.dm_display_frequency = hz;
                dm.dm_fields |= DM_DISPLAYFREQUENCY;
            }
        }
        let right_edge = list()
            .iter()
            .filter(|d| d.active)
            .filter_map(|d| current_settings(&d.id))
            .filter(|dm| dm.dm_pels_width > 0)
            .map(|dm| unsafe { dm.u.position.x } + dm.dm_pels_width as i32)
            .max()
            .unwrap_or(0);
        dm.dm_fields |= DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT;
        dm.u.position = POINTL { x: right_edge, y: 0 }; // writing a union field is safe; only reads are unsafe
        apply(Some(id), &mut dm)
    } else {
        // detach: zero-sized mode is the documented "pull the cable" trick
        let mut dm = DEVMODEW::new();
        dm.dm_fields = DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT;
        dm.dm_pels_width = 0;
        dm.dm_pels_height = 0;
        apply(Some(id), &mut dm)
    }
}
