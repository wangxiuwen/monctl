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
const DISPLAY_DEVICE_ACTIVE: DWORD = 0x0000_0010;
const DISPLAY_DEVICE_PRIMARY_DEVICE: DWORD = 0x0000_0004;
const DM_POSITION: DWORD = 0x0000_0020;
const DM_PELSWIDTH: DWORD = 0x0008_0000;
const DM_PELSHEIGHT: DWORD = 0x0010_0000;
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

#[repr(C)]
union DevModeU {
    printer: DevModePrinter,
    display_frequency: DWORD,
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
    dm_position: POINTL,
    dm_display_orientation: DWORD,
    dm_display_fixed_output: DWORD,
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
        if flags & 0x0000_0020 != 0 {
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

pub fn list() -> Vec<DisplayInfo> {
    adapters()
        .into_iter()
        .map(|(device, description, flags)| {
            let active = flags & DISPLAY_DEVICE_ACTIVE != 0;
            let main = flags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0;
            let detail = current_settings(&device)
                .filter(|_| active)
                .map(|dm| {
                    let hz = unsafe { dm.u.display_frequency };
                    let r = if hz > 1 { format!(" @{}Hz", hz) } else { String::new() };
                    format!("{}x{}{} ({},{})",
                        dm.dm_pels_width, dm.dm_pels_height, r, dm.dm_position.x, dm.dm_position.y)
                })
                .unwrap_or_else(|| "n/a".into());
            DisplayInfo {
                id: device,
                active,
                main,
                name: if description.is_empty() { "Display".into() } else { description },
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

pub fn set_enabled(id: &str, enable: bool) -> Result<(), String> {
    if !id.starts_with("\\\\.\\DISPLAY") {
        return Err(format!("'{id}' is not a valid Windows display id (expected \\\\.\\DISPLAYn, see `monctl list`)"));
    }
    if enable {
        // attach: restore the stored mode, positioned to the right of the current desktop
        let mut dm = current_settings(id).ok_or_else(|| format!("no stored mode for {id} — cannot re-attach it"))?;
        let right_edge = list()
            .iter()
            .filter(|d| d.active)
            .filter_map(|d| current_settings(&d.id))
            .map(|dm| dm.dm_position.x + dm.dm_pels_width as i32)
            .max()
            .unwrap_or(0);
        dm.dm_fields = DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT;
        dm.dm_position = POINTL { x: right_edge, y: 0 };
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
