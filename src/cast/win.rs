// Windows side of `monctl cast`: each tablet gets a monitor of the bundled
// virtual display driver (winvdd.rs) at its exact size. A capture thread
// reads it with DXGI Desktop Duplication (frames only when something
// changed) into a "latest frame" slot as NV12; an encoder thread feeds Media
// Foundation's H.264 encoder - the GPU's hardware one when there is one
// (NVENC/QSV/AMF behind an async MFT), Microsoft's software one otherwise -
// and repackages its Annex-B output as the AVCC the page expects. Touches
// come back as SendInput.
//
// All COM is called through hand-written vtables (indices checked against
// the SDK/Wine IDL); the crate stays dependency-free.

use super::{annexb_to_avcc, avcc, Hello, Options, Packet, Screen};
use crate::winvdd;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

type HRESULT = i32;
type Ptr = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct GUID(u32, u16, u16, [u8; 8]);

const IID_IDXGI_FACTORY1: GUID = GUID(0x770aae78, 0xf26f, 0x4dba, [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87]);
const IID_IDXGI_OUTPUT1: GUID = GUID(0x00cddea8, 0x939b, 0x4b83, [0xa3, 0x40, 0xa6, 0x85, 0x22, 0x66, 0x66, 0xcc]);
const IID_ID3D11_TEXTURE2D: GUID = GUID(0x6f15aaf2, 0xd208, 0x4e89, [0x9a, 0xb4, 0x48, 0x95, 0x35, 0xd3, 0x4f, 0x9c]);
const IID_IMF_TRANSFORM: GUID = GUID(0xbf94c121, 0x5b05, 0x4e6f, [0x80, 0x00, 0xba, 0x59, 0x89, 0x61, 0x41, 0x4d]);
const IID_IMF_MEDIA_EVENT_GENERATOR: GUID = GUID(0x2cd0bd52, 0xbcd5, 0x4b89, [0xb6, 0x2c, 0xea, 0xdc, 0x0c, 0x03, 0x1e, 0x7d]);
const IID_ICODEC_API: GUID = GUID(0x901db4c7, 0x31ce, 0x41a2, [0x85, 0xdc, 0x8f, 0xa0, 0xbf, 0x41, 0xb8, 0xda]);

const MFT_CATEGORY_VIDEO_ENCODER: GUID = GUID(0xf79eac7d, 0xe545, 0x4387, [0xbd, 0xee, 0xd6, 0x47, 0xd7, 0xbd, 0xe4, 0x2a]);
const MF_MEDIATYPE_VIDEO: GUID = GUID(0x73646976, 0x0000, 0x0010, [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
const fn fourcc_subtype(f: &[u8; 4]) -> GUID {
    GUID(u32::from_le_bytes(*f), 0x0000, 0x0010, [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71])
}
const MF_VIDEO_FORMAT_H264: GUID = fourcc_subtype(b"H264");
const MF_VIDEO_FORMAT_NV12: GUID = fourcc_subtype(b"NV12");
const MF_MT_MAJOR_TYPE: GUID = GUID(0x48eba18e, 0xf8c9, 0x4687, [0xbf, 0x11, 0x0a, 0x74, 0xc9, 0xf9, 0x6a, 0x8f]);
const MF_MT_SUBTYPE: GUID = GUID(0xf7e34c9a, 0x42e8, 0x4714, [0xb7, 0x4b, 0xcb, 0x29, 0xd7, 0x2c, 0x35, 0xe5]);
const MF_MT_FRAME_SIZE: GUID = GUID(0x1652c33d, 0xd6b2, 0x4012, [0xb8, 0x34, 0x72, 0x03, 0x08, 0x49, 0xa3, 0x7d]);
const MF_MT_FRAME_RATE: GUID = GUID(0xc459a2e8, 0x3d2c, 0x4e44, [0xb1, 0x32, 0xfe, 0xe5, 0x15, 0x6c, 0x7b, 0xb0]);
const MF_MT_PIXEL_ASPECT_RATIO: GUID = GUID(0xc6376a1e, 0x8d0a, 0x4027, [0xbe, 0x45, 0x6d, 0x9a, 0x0a, 0xd3, 0x9b, 0xb6]);
const MF_MT_AVG_BITRATE: GUID = GUID(0x20332624, 0xfb0d, 0x4d9e, [0xbd, 0x0d, 0xcb, 0xf6, 0x78, 0x6c, 0x10, 0x2e]);
const MF_MT_INTERLACE_MODE: GUID = GUID(0xe2724bb8, 0xe676, 0x4806, [0xb4, 0xb2, 0xa8, 0xd6, 0xef, 0xb4, 0x4c, 0xcd]);
const MF_MT_MPEG2_PROFILE: GUID = GUID(0xad76a80b, 0x2d5c, 0x4e0b, [0xb3, 0x75, 0x64, 0xe5, 0x20, 0x13, 0x70, 0x36]);
const MF_LOW_LATENCY: GUID = GUID(0x9c27891a, 0xed7a, 0x40e1, [0x88, 0xe8, 0xb2, 0x27, 0x27, 0xa0, 0x24, 0xee]); // = CODECAPI_AVLowLatencyMode
const MF_TRANSFORM_ASYNC_UNLOCK: GUID = GUID(0xe5666d6b, 0x3422, 0x4eb6, [0xa4, 0x21, 0xda, 0x7d, 0xb1, 0xf8, 0xe2, 0x07]);
const MF_SAMPLE_CLEAN_POINT: GUID = GUID(0x9cdf01d8, 0xa0f0, 0x43ba, [0xb0, 0x77, 0xea, 0xa0, 0x6c, 0xbd, 0x72, 0x8a]);
const CODECAPI_FORCE_KEYFRAME: GUID = GUID(0x398c1b98, 0x8353, 0x475a, [0x9e, 0xf2, 0x8f, 0x26, 0x5d, 0x26, 0x03, 0x45]);
const CODECAPI_GOP_SIZE: GUID = GUID(0x95f31b26, 0x95a4, 0x41aa, [0x93, 0x03, 0x24, 0x6a, 0x7f, 0xc6, 0xee, 0xf1]);

const DXGI_ERROR_NOT_FOUND: HRESULT = 0x887A0002u32 as i32;
const DXGI_ERROR_WAIT_TIMEOUT: HRESULT = 0x887A0027u32 as i32;
const MF_E_TRANSFORM_NEED_MORE_INPUT: HRESULT = 0xC00D6D72u32 as i32;
const MF_E_TRANSFORM_STREAM_CHANGE: HRESULT = 0xC00D6D61u32 as i32;

#[link(name = "dxgi")]
extern "system" {
    fn CreateDXGIFactory1(riid: *const GUID, factory: *mut Ptr) -> HRESULT;
}

#[link(name = "d3d11")]
extern "system" {
    fn D3D11CreateDevice(adapter: Ptr, driver: u32, software: Ptr, flags: u32, levels: *const u32, n: u32, sdk: u32, device: *mut Ptr, level: *mut u32, ctx: *mut Ptr) -> HRESULT;
}

#[repr(C)]
struct MftRegisterTypeInfo {
    major: GUID,
    sub: GUID,
}

#[link(name = "mfplat")]
extern "system" {
    fn MFStartup(version: u32, flags: u32) -> HRESULT;
    fn MFTEnumEx(category: GUID, flags: u32, input: *const MftRegisterTypeInfo, output: *const MftRegisterTypeInfo, activates: *mut *mut Ptr, count: *mut u32) -> HRESULT;
    fn MFCreateMediaType(t: *mut Ptr) -> HRESULT;
    fn MFCreateSample(s: *mut Ptr) -> HRESULT;
    fn MFCreateMemoryBuffer(len: u32, b: *mut Ptr) -> HRESULT;
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: Ptr, flags: u32) -> HRESULT;
    fn CoTaskMemFree(p: Ptr);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: i32) -> Ptr;
    fn SetHandleInformation(h: Ptr, mask: u32, flags: u32) -> i32;
    fn GetConsoleMode(h: Ptr, mode: *mut u32) -> i32;
    fn SetConsoleMode(h: Ptr, mode: u32) -> i32;
    fn SetConsoleCtrlHandler(handler: extern "system" fn(u32) -> i32, add: i32) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn SendInput(n: u32, inputs: *const INPUT, size: i32) -> u32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
}

// ------------------------------------------------------------ COM glue ----

/// vtable slot `i` of a COM object, as the given function pointer type.
unsafe fn slot<F: Copy>(obj: Ptr, i: usize) -> F {
    let vtbl = *(obj as *const *const usize);
    std::mem::transmute_copy(&*vtbl.add(i))
}

unsafe fn release(obj: Ptr) {
    if !obj.is_null() {
        slot::<unsafe extern "system" fn(Ptr) -> u32>(obj, 2)(obj);
    }
}

unsafe fn query(obj: Ptr, iid: &GUID) -> Result<Ptr, HRESULT> {
    let mut out: Ptr = std::ptr::null_mut();
    let hr = slot::<unsafe extern "system" fn(Ptr, *const GUID, *mut Ptr) -> HRESULT>(obj, 0)(obj, iid, &mut out);
    if hr < 0 {
        Err(hr)
    } else {
        Ok(out)
    }
}

fn check(hr: HRESULT, what: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{what} failed (0x{:08x})", hr as u32))
    } else {
        Ok(())
    }
}

/// Owned COM pointer.
struct Com(Ptr);

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { release(self.0) }
    }
}

unsafe impl Send for Com {}

// IMFAttributes (also IMFMediaType / IMFSample / IMFActivate) setters
unsafe fn set_u32(attrs: Ptr, key: &GUID, v: u32) -> HRESULT {
    slot::<unsafe extern "system" fn(Ptr, *const GUID, u32) -> HRESULT>(attrs, 21)(attrs, key, v)
}
unsafe fn set_u64(attrs: Ptr, key: &GUID, v: u64) -> HRESULT {
    slot::<unsafe extern "system" fn(Ptr, *const GUID, u64) -> HRESULT>(attrs, 22)(attrs, key, v)
}
unsafe fn set_guid(attrs: Ptr, key: &GUID, v: &GUID) -> HRESULT {
    slot::<unsafe extern "system" fn(Ptr, *const GUID, *const GUID) -> HRESULT>(attrs, 24)(attrs, key, v)
}
unsafe fn get_u32(attrs: Ptr, key: &GUID) -> Option<u32> {
    let mut v = 0u32;
    (slot::<unsafe extern "system" fn(Ptr, *const GUID, *mut u32) -> HRESULT>(attrs, 7)(attrs, key, &mut v) >= 0).then_some(v)
}

#[repr(C)]
struct VARIANT {
    vt: u16,
    reserved: [u16; 3],
    val: u64,
    extra: u64,
}

/// ICodecAPI::SetValue with a VT_UI4 (VT_BOOL when `boolean`).
unsafe fn codec_set(api: Ptr, key: &GUID, v: u32, boolean: bool) {
    const VT_UI4: u16 = 19;
    const VT_BOOL: u16 = 11;
    let mut var = VARIANT { vt: if boolean { VT_BOOL } else { VT_UI4 }, reserved: [0; 3], val: if boolean { if v != 0 { 0xFFFF } else { 0 } } else { v as u64 }, extra: 0 };
    slot::<unsafe extern "system" fn(Ptr, *const GUID, *mut VARIANT) -> HRESULT>(api, 9)(api, key, &mut var);
}

// ------------------------------------------------------- frame handoff ----

/// The newest captured frame (NV12). The encoder always takes the latest;
/// frames it couldn't get to are simply replaced - latency over completeness.
struct Slot {
    frame: Mutex<Option<Frame>>,
    ready: Condvar,
}

#[derive(Clone)]
struct Frame {
    nv12: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    seq: u64,
}

struct Shared {
    slot: Slot,
    stop: AtomicBool,
    force_key: AtomicBool,
    /// which VDD monitor this tablet is, re-read after every driver reload
    display: Mutex<String>,
}

// ------------------------------------------------------------ capture ----

#[repr(C)]
#[derive(Clone, Copy)]
struct RECT {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

#[repr(C)]
struct DxgiOutputDesc {
    device_name: [u16; 32],
    coords: RECT,
    attached: i32,
    rotation: u32,
    monitor: Ptr,
}

#[repr(C)]
struct FrameInfo {
    last_present: i64,
    last_mouse: i64,
    accumulated: u32,
    rects_coalesced: i32,
    protected_masked: i32,
    pointer_x: i32,
    pointer_y: i32,
    pointer_visible: i32,
    metadata_size: u32,
    pointer_shape_size: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Texture2dDesc {
    width: u32,
    height: u32,
    mip_levels: u32,
    array_size: u32,
    format: u32,
    sample_count: u32,
    sample_quality: u32,
    usage: u32,
    bind_flags: u32,
    cpu_access: u32,
    misc: u32,
}

#[repr(C)]
struct Mapped {
    data: *const u8,
    row_pitch: u32,
    depth_pitch: u32,
}

/// Desktop Duplication of one output.
struct Duplication {
    _device: Com,
    context: Com,
    dup: Com,
    staging: Option<(Com, u32, u32)>,
}

impl Duplication {
    /// Find `\\.\DISPLAYn` among all adapters' outputs and duplicate it.
    fn open(display: &str) -> Result<Duplication, String> {
        unsafe {
            let mut factory: Ptr = std::ptr::null_mut();
            check(CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory), "CreateDXGIFactory1")?;
            let factory = Com(factory);
            for a in 0.. {
                let mut adapter: Ptr = std::ptr::null_mut();
                let hr = slot::<unsafe extern "system" fn(Ptr, u32, *mut Ptr) -> HRESULT>(factory.0, 12)(factory.0, a, &mut adapter);
                if hr == DXGI_ERROR_NOT_FOUND {
                    break;
                }
                if hr < 0 {
                    continue;
                }
                let adapter = Com(adapter);
                for o in 0.. {
                    let mut output: Ptr = std::ptr::null_mut();
                    let hr = slot::<unsafe extern "system" fn(Ptr, u32, *mut Ptr) -> HRESULT>(adapter.0, 7)(adapter.0, o, &mut output);
                    if hr == DXGI_ERROR_NOT_FOUND {
                        break;
                    }
                    if hr < 0 {
                        continue;
                    }
                    let output = Com(output);
                    let mut desc: DxgiOutputDesc = std::mem::zeroed();
                    slot::<unsafe extern "system" fn(Ptr, *mut DxgiOutputDesc) -> HRESULT>(output.0, 7)(output.0, &mut desc);
                    let len = desc.device_name.iter().position(|&c| c == 0).unwrap_or(32);
                    if String::from_utf16_lossy(&desc.device_name[..len]) != display {
                        continue;
                    }
                    // device on the adapter that owns the output
                    let (mut device, mut context, mut level): (Ptr, Ptr, u32) = (std::ptr::null_mut(), std::ptr::null_mut(), 0);
                    const D3D_DRIVER_TYPE_UNKNOWN: u32 = 0;
                    const D3D11_CREATE_DEVICE_BGRA_SUPPORT: u32 = 0x20;
                    check(
                        D3D11CreateDevice(adapter.0, D3D_DRIVER_TYPE_UNKNOWN, std::ptr::null_mut(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, std::ptr::null(), 0, 7, &mut device, &mut level, &mut context),
                        "D3D11CreateDevice",
                    )?;
                    let (device, context) = (Com(device), Com(context));
                    let output1 = Com(query(output.0, &IID_IDXGI_OUTPUT1).map_err(|hr| format!("IDXGIOutput1 unavailable (0x{:08x})", hr as u32))?);
                    let mut dup: Ptr = std::ptr::null_mut();
                    check(slot::<unsafe extern "system" fn(Ptr, Ptr, *mut Ptr) -> HRESULT>(output1.0, 22)(output1.0, device.0, &mut dup), "DuplicateOutput")?;
                    return Ok(Duplication { _device: device, context, dup: Com(dup), staging: None });
                }
            }
            Err(format!("{display} is not an output of any graphics adapter"))
        }
    }

    /// Next changed frame as BGRA rows → NV12. Ok(None) = nothing new yet.
    fn next(&mut self, timeout_ms: u32, seq: u64) -> Result<Option<Frame>, HRESULT> {
        unsafe {
            let mut info: FrameInfo = std::mem::zeroed();
            let mut resource: Ptr = std::ptr::null_mut();
            let hr = slot::<unsafe extern "system" fn(Ptr, u32, *mut FrameInfo, *mut Ptr) -> HRESULT>(self.dup.0, 8)(self.dup.0, timeout_ms, &mut info, &mut resource);
            if hr == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(None);
            }
            if hr < 0 {
                return Err(hr);
            }
            let resource = Com(resource);
            let release_frame = |d: &Duplication| {
                slot::<unsafe extern "system" fn(Ptr) -> HRESULT>(d.dup.0, 14)(d.dup.0);
            };
            // pointer-only updates carry no new image
            if info.last_present == 0 && seq > 0 {
                release_frame(self);
                return Ok(None);
            }
            let tex = match query(resource.0, &IID_ID3D11_TEXTURE2D) {
                Ok(t) => Com(t),
                Err(hr) => {
                    release_frame(self);
                    return Err(hr);
                }
            };
            let mut desc: Texture2dDesc = std::mem::zeroed();
            slot::<unsafe extern "system" fn(Ptr, *mut Texture2dDesc)>(tex.0, 10)(tex.0, &mut desc);
            let (w, h) = (desc.width & !1, desc.height & !1);
            if self.staging.as_ref().is_none_or(|s| s.1 != desc.width || s.2 != desc.height) {
                let mut sd = desc;
                sd.mip_levels = 1;
                sd.array_size = 1;
                sd.sample_count = 1;
                sd.sample_quality = 0;
                sd.usage = 3; // D3D11_USAGE_STAGING
                sd.bind_flags = 0;
                sd.cpu_access = 0x20000; // D3D11_CPU_ACCESS_READ
                sd.misc = 0;
                let mut device: Ptr = std::ptr::null_mut();
                slot::<unsafe extern "system" fn(Ptr, *mut Ptr)>(self.context.0, 3)(self.context.0, &mut device); // GetDevice
                let device = Com(device);
                let mut staging: Ptr = std::ptr::null_mut();
                let hr = slot::<unsafe extern "system" fn(Ptr, *const Texture2dDesc, Ptr, *mut Ptr) -> HRESULT>(device.0, 5)(device.0, &sd, std::ptr::null_mut(), &mut staging);
                if hr < 0 {
                    release_frame(self);
                    return Err(hr);
                }
                self.staging = Some((Com(staging), desc.width, desc.height));
            }
            let staging = self.staging.as_ref().unwrap().0 .0;
            slot::<unsafe extern "system" fn(Ptr, Ptr, Ptr)>(self.context.0, 47)(self.context.0, staging, tex.0); // CopyResource
            release_frame(self);
            let mut mapped: Mapped = std::mem::zeroed();
            let hr = slot::<unsafe extern "system" fn(Ptr, Ptr, u32, u32, u32, *mut Mapped) -> HRESULT>(self.context.0, 14)(self.context.0, staging, 0, 1, 0, &mut mapped);
            if hr < 0 {
                return Err(hr);
            }
            let rows = std::slice::from_raw_parts(mapped.data, mapped.row_pitch as usize * desc.height as usize);
            let nv12 = bgra_to_nv12(rows, mapped.row_pitch as usize, w as usize, h as usize);
            slot::<unsafe extern "system" fn(Ptr, Ptr, u32)>(self.context.0, 15)(self.context.0, staging, 0); // Unmap
            Ok(Some(Frame { nv12: Arc::new(nv12), width: w, height: h, seq: seq + 1 }))
        }
    }
}

/// BGRA → NV12, BT.709 limited range (chroma from each 2x2 block's average).
fn bgra_to_nv12(src: &[u8], pitch: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 3 / 2];
    let (y_plane, uv_plane) = out.split_at_mut(w * h);
    for y in 0..h {
        let row = &src[y * pitch..y * pitch + w * 4];
        let dst = &mut y_plane[y * w..(y + 1) * w];
        for (x, px) in row.chunks_exact(4).enumerate() {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            dst[x] = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
        }
    }
    for y in (0..h).step_by(2) {
        let r0 = &src[y * pitch..];
        let r1 = &src[(y + 1) * pitch..];
        let dst = &mut uv_plane[(y / 2) * w..(y / 2) * w + w];
        for x in (0..w).step_by(2) {
            let i = x * 4;
            let avg = |c: usize| (r0[i + c] as i32 + r0[i + 4 + c] as i32 + r1[i + c] as i32 + r1[i + 4 + c] as i32 + 2) >> 2;
            let (b, g, r) = (avg(0), avg(1), avg(2));
            dst[x] = (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            dst[x + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    out
}

fn capture_loop(shared: Arc<Shared>) {
    let mut seq = 0u64;
    let mut dup: Option<Duplication> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        if dup.is_none() {
            let display = shared.display.lock().unwrap().clone();
            match Duplication::open(&display) {
                Ok(d) => dup = Some(d),
                Err(_) => {
                    // driver reloading, secure desktop (UAC / lock screen): retry
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
            }
        }
        match dup.as_mut().unwrap().next(100, seq) {
            Ok(Some(f)) => {
                seq = f.seq;
                *shared.slot.frame.lock().unwrap() = Some(f);
                shared.slot.ready.notify_one();
            }
            Ok(None) => {}
            // ACCESS_LOST (mode change, driver reload, desktop switch): start over
            Err(_) => dup = None,
        }
    }
}

// ------------------------------------------------------------ encoder ----

struct Encoder {
    mft: Com,
    events: Option<Com>, // async (hardware) MFTs talk through events
    codec: Option<Com>,
    provides_samples: bool,
    out_size: u32,
    width: u32,
    height: u32,
}

impl Encoder {
    fn new(width: u32, height: u32, bitrate: u32) -> Result<Encoder, String> {
        unsafe {
            CoInitializeEx(std::ptr::null_mut(), 0); // MTA
            check(MFStartup(0x0002_0070, 0), "MFStartup")?;
            const HARDWARE: u32 = 0x4;
            const SYNCMFT: u32 = 0x1;
            const SORTANDFILTER: u32 = 0x40;
            let mut last = String::from("no H.264 encoder found");
            for flags in [HARDWARE | SORTANDFILTER, SYNCMFT | SORTANDFILTER] {
                let input = MftRegisterTypeInfo { major: MF_MEDIATYPE_VIDEO, sub: MF_VIDEO_FORMAT_NV12 };
                let output = MftRegisterTypeInfo { major: MF_MEDIATYPE_VIDEO, sub: MF_VIDEO_FORMAT_H264 };
                let (mut list, mut n): (*mut Ptr, u32) = (std::ptr::null_mut(), 0);
                if MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, &input, &output, &mut list, &mut n) < 0 || list.is_null() {
                    continue;
                }
                let activates: Vec<Com> = (0..n as usize).map(|i| Com(*list.add(i))).collect();
                CoTaskMemFree(list.cast());
                for act in &activates {
                    let mut mft: Ptr = std::ptr::null_mut();
                    // IMFActivate::ActivateObject
                    if slot::<unsafe extern "system" fn(Ptr, *const GUID, *mut Ptr) -> HRESULT>(act.0, 33)(act.0, &IID_IMF_TRANSFORM, &mut mft) < 0 {
                        continue;
                    }
                    match Self::setup(Com(mft), flags & HARDWARE != 0, width, height, bitrate) {
                        Ok(e) => return Ok(e),
                        Err(e) => last = e,
                    }
                }
            }
            Err(last)
        }
    }

    unsafe fn media_type(subtype: &GUID, width: u32, height: u32, bitrate: u32) -> Result<Com, String> {
        let mut t: Ptr = std::ptr::null_mut();
        check(MFCreateMediaType(&mut t), "MFCreateMediaType")?;
        let t = Com(t);
        set_guid(t.0, &MF_MT_MAJOR_TYPE, &MF_MEDIATYPE_VIDEO);
        set_guid(t.0, &MF_MT_SUBTYPE, subtype);
        set_u64(t.0, &MF_MT_FRAME_SIZE, (width as u64) << 32 | height as u64);
        set_u64(t.0, &MF_MT_FRAME_RATE, 60u64 << 32 | 1);
        set_u64(t.0, &MF_MT_PIXEL_ASPECT_RATIO, 1u64 << 32 | 1);
        set_u32(t.0, &MF_MT_INTERLACE_MODE, 2); // progressive
        if *subtype == MF_VIDEO_FORMAT_H264 {
            set_u32(t.0, &MF_MT_AVG_BITRATE, bitrate);
            set_u32(t.0, &MF_MT_MPEG2_PROFILE, 77); // Main: what every browser decodes in hardware
        }
        Ok(t)
    }

    unsafe fn setup(mft: Com, hardware: bool, width: u32, height: u32, bitrate: u32) -> Result<Encoder, String> {
        let mut attrs: Ptr = std::ptr::null_mut();
        if slot::<unsafe extern "system" fn(Ptr, *mut Ptr) -> HRESULT>(mft.0, 8)(mft.0, &mut attrs) >= 0 && !attrs.is_null() {
            let attrs = Com(attrs);
            if hardware {
                set_u32(attrs.0, &MF_TRANSFORM_ASYNC_UNLOCK, 1);
            }
            set_u32(attrs.0, &MF_LOW_LATENCY, 1);
        }
        // encoders want the output type first
        let out = Self::media_type(&MF_VIDEO_FORMAT_H264, width, height, bitrate)?;
        check(slot::<unsafe extern "system" fn(Ptr, u32, Ptr, u32) -> HRESULT>(mft.0, 16)(mft.0, 0, out.0, 0), "SetOutputType")?;
        let inp = Self::media_type(&MF_VIDEO_FORMAT_NV12, width, height, bitrate)?;
        check(slot::<unsafe extern "system" fn(Ptr, u32, Ptr, u32) -> HRESULT>(mft.0, 15)(mft.0, 0, inp.0, 0), "SetInputType")?;
        let codec = query(mft.0, &IID_ICODEC_API).ok().map(Com);
        if let Some(c) = &codec {
            codec_set(c.0, &MF_LOW_LATENCY, 1, true);
            codec_set(c.0, &CODECAPI_GOP_SIZE, 600, false); // keyframes on demand, plus a slow safety net
        }
        #[repr(C)]
        struct StreamInfo {
            flags: u32,
            size: u32,
            align: u32,
        }
        let mut info: StreamInfo = std::mem::zeroed();
        slot::<unsafe extern "system" fn(Ptr, u32, *mut StreamInfo) -> HRESULT>(mft.0, 7)(mft.0, 0, &mut info);
        let events = if hardware { Some(Com(query(mft.0, &IID_IMF_MEDIA_EVENT_GENERATOR).map_err(|_| "async encoder without events".to_string())?)) } else { None };
        const BEGIN_STREAMING: u32 = 0x1000_0000;
        const START_OF_STREAM: u32 = 0x1000_0003;
        let msg = slot::<unsafe extern "system" fn(Ptr, u32, usize) -> HRESULT>(mft.0, 23);
        msg(mft.0, BEGIN_STREAMING, 0);
        msg(mft.0, START_OF_STREAM, 0);
        Ok(Encoder { mft, events, codec, provides_samples: info.flags & 0x300 != 0, out_size: info.size.max(1 << 20), width, height })
    }

    fn force_keyframe(&self) {
        if let Some(c) = &self.codec {
            unsafe { codec_set(c.0, &CODECAPI_FORCE_KEYFRAME, 1, false) };
        }
    }

    /// Feed one frame; `n` numbers input samples (their timestamps).
    unsafe fn input(&self, f: &Frame, n: u64) -> HRESULT {
        let (mut sample, mut buffer): (Ptr, Ptr) = (std::ptr::null_mut(), std::ptr::null_mut());
        if MFCreateSample(&mut sample) < 0 || MFCreateMemoryBuffer(f.nv12.len() as u32, &mut buffer) < 0 {
            return -1;
        }
        let (sample, buffer) = (Com(sample), Com(buffer));
        let mut p: *mut u8 = std::ptr::null_mut();
        slot::<unsafe extern "system" fn(Ptr, *mut *mut u8, *mut u32, *mut u32) -> HRESULT>(buffer.0, 3)(buffer.0, &mut p, std::ptr::null_mut(), std::ptr::null_mut());
        std::ptr::copy_nonoverlapping(f.nv12.as_ptr(), p, f.nv12.len());
        slot::<unsafe extern "system" fn(Ptr) -> HRESULT>(buffer.0, 4)(buffer.0);
        slot::<unsafe extern "system" fn(Ptr, u32) -> HRESULT>(buffer.0, 6)(buffer.0, f.nv12.len() as u32);
        slot::<unsafe extern "system" fn(Ptr, Ptr) -> HRESULT>(sample.0, 42)(sample.0, buffer.0);
        slot::<unsafe extern "system" fn(Ptr, i64) -> HRESULT>(sample.0, 36)(sample.0, n as i64 * 166_667); // 100 ns units, nominal 60 fps
        slot::<unsafe extern "system" fn(Ptr, i64) -> HRESULT>(sample.0, 38)(sample.0, 166_667);
        slot::<unsafe extern "system" fn(Ptr, u32, Ptr, u32) -> HRESULT>(self.mft.0, 24)(self.mft.0, 0, sample.0, 0)
    }

    /// One ProcessOutput: Ok(Some((annexb, key))) / Ok(None) when it needs input.
    unsafe fn output(&self) -> Result<Option<(Vec<u8>, bool)>, HRESULT> {
        #[repr(C)]
        struct OutputDataBuffer {
            stream: u32,
            sample: Ptr,
            status: u32,
            events: Ptr,
        }
        let mut own: Option<Com> = None;
        if !self.provides_samples {
            let (mut s, mut b): (Ptr, Ptr) = (std::ptr::null_mut(), std::ptr::null_mut());
            MFCreateSample(&mut s);
            MFCreateMemoryBuffer(self.out_size, &mut b);
            let b = Com(b);
            slot::<unsafe extern "system" fn(Ptr, Ptr) -> HRESULT>(s, 42)(s, b.0);
            own = Some(Com(s));
        }
        let mut db = OutputDataBuffer { stream: 0, sample: own.as_ref().map_or(std::ptr::null_mut(), |s| s.0), status: 0, events: std::ptr::null_mut() };
        let mut status = 0u32;
        let hr = slot::<unsafe extern "system" fn(Ptr, u32, u32, *mut OutputDataBuffer, *mut u32) -> HRESULT>(self.mft.0, 25)(self.mft.0, 0, 1, &mut db, &mut status);
        release(db.events);
        if hr == MF_E_TRANSFORM_NEED_MORE_INPUT {
            return Ok(None);
        }
        if hr == MF_E_TRANSFORM_STREAM_CHANGE {
            // re-assert our output type and carry on
            if let Ok(t) = Self::media_type(&MF_VIDEO_FORMAT_H264, self.width, self.height, 0) {
                slot::<unsafe extern "system" fn(Ptr, u32, Ptr, u32) -> HRESULT>(self.mft.0, 16)(self.mft.0, 0, t.0, 0);
            }
            return Ok(None);
        }
        if hr < 0 {
            return Err(hr);
        }
        let sample = if self.provides_samples { Com(db.sample) } else { own.take().unwrap() };
        let key = get_u32(sample.0, &MF_SAMPLE_CLEAN_POINT) == Some(1);
        let mut buf: Ptr = std::ptr::null_mut();
        slot::<unsafe extern "system" fn(Ptr, *mut Ptr) -> HRESULT>(sample.0, 41)(sample.0, &mut buf);
        let buf = Com(buf);
        let (mut p, mut len): (*mut u8, u32) = (std::ptr::null_mut(), 0);
        slot::<unsafe extern "system" fn(Ptr, *mut *mut u8, *mut u32, *mut u32) -> HRESULT>(buf.0, 3)(buf.0, &mut p, std::ptr::null_mut(), &mut len);
        let data = std::slice::from_raw_parts(p, len as usize).to_vec();
        slot::<unsafe extern "system" fn(Ptr) -> HRESULT>(buf.0, 4)(buf.0);
        Ok(Some((data, key)))
    }

    /// Next async event type (METransformNeedInput = 601, METransformHaveOutput
    /// = 602), polling so a stopping session never hangs in GetEvent.
    unsafe fn next_event(&self, wait: Duration, stop: &AtomicBool) -> Option<u32> {
        const MF_EVENT_FLAG_NO_WAIT: u32 = 1;
        const MF_E_NO_EVENTS_AVAILABLE: HRESULT = 0xC00D3E80u32 as i32;
        let ev = self.events.as_ref()?;
        let until = Instant::now() + wait;
        loop {
            let mut e: Ptr = std::ptr::null_mut();
            let hr = slot::<unsafe extern "system" fn(Ptr, u32, *mut Ptr) -> HRESULT>(ev.0, 3)(ev.0, MF_EVENT_FLAG_NO_WAIT, &mut e);
            if hr >= 0 {
                let e = Com(e);
                let mut t = 0u32;
                slot::<unsafe extern "system" fn(Ptr, *mut u32) -> HRESULT>(e.0, 33)(e.0, &mut t);
                return Some(t);
            }
            if hr != MF_E_NO_EVENTS_AVAILABLE || Instant::now() >= until || stop.load(Ordering::Relaxed) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Hands encoded access units to the WebSocket writer.
struct Out {
    tx: SyncSender<Packet>,
    params: Vec<u8>,
    frames: u64,
}

impl Out {
    /// false once the session is gone
    fn send(&mut self, annexb: &[u8], width: u32, height: u32, shared: &Shared) -> bool {
        let (data, params, key) = annexb_to_avcc(annexb);
        if let Some((sps, pps)) = params {
            let both = [sps.as_slice(), pps.as_slice()].concat();
            if both != self.params && sps.len() >= 4 {
                self.params = both;
                let codec = format!("avc1.{:02x}{:02x}{:02x}", sps[1], sps[2], sps[3]);
                if self.tx.send(Packet::Config { codec, avcc: avcc(&sps, &pps), width, height }).is_err() {
                    return false;
                }
            }
        }
        if data.is_empty() {
            return true;
        }
        self.frames += 1;
        match self.tx.try_send(Packet::Frame { key, ts_us: self.frames * 16_667, data }) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                shared.force_key.store(true, Ordering::Relaxed); // the page lost a reference frame
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

fn encode_loop(shared: Arc<Shared>, tx: SyncSender<Packet>, bitrate: u32) {
    let mut out = Out { tx, params: vec![], frames: 0 };
    let mut enc: Option<Encoder> = None;
    let mut last_seq = 0u64;
    let mut fed = 0u64;
    let mut wants_input = 0u32; // async MFTs: METransformNeedInput not yet answered
    while !shared.stop.load(Ordering::Relaxed) {
        // newest frame; a keyframe request on a still screen re-sends the last one
        let frame = {
            let mut g = shared.slot.frame.lock().unwrap();
            let deadline = Instant::now() + Duration::from_millis(200);
            loop {
                if let Some(f) = g.as_ref() {
                    if f.seq != last_seq || shared.force_key.load(Ordering::Relaxed) {
                        break Some(f.clone());
                    }
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() || shared.stop.load(Ordering::Relaxed) {
                    break None;
                }
                g = shared.slot.ready.wait_timeout(g, left).unwrap().0;
            }
        };
        let Some(frame) = frame else { continue };
        last_seq = frame.seq;
        if enc.as_ref().is_some_and(|e| e.width != frame.width || e.height != frame.height) {
            enc = None; // screen resized: new encoder, new stream config
        }
        if enc.is_none() {
            match Encoder::new(frame.width, frame.height, bitrate) {
                Ok(e) => {
                    enc = Some(e);
                    out.params.clear();
                    wants_input = 0;
                }
                Err(e) => {
                    eprintln!("monctl cast: {e}");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
            shared.force_key.store(true, Ordering::Relaxed);
        }
        let e = enc.as_ref().unwrap();
        if shared.force_key.swap(false, Ordering::Relaxed) {
            e.force_keyframe();
        }
        let ok = unsafe {
            if e.events.is_some() {
                // async: answer a pending input request (collecting output meanwhile) ...
                let mut alive = true;
                while wants_input == 0 && alive && !shared.stop.load(Ordering::Relaxed) {
                    match e.next_event(Duration::from_millis(500), &shared.stop) {
                        Some(601) => wants_input += 1,
                        Some(602) => {
                            if let Ok(Some((d, _))) = e.output() {
                                alive = out.send(&d, e.width, e.height, &shared);
                            }
                        }
                        _ => {}
                    }
                }
                if wants_input > 0 {
                    wants_input -= 1;
                    fed += 1;
                    e.input(&frame, fed);
                }
                // ... then give this frame's output a moment, so a screen that
                // just went still shows its final state now, not on the next change
                let until = Instant::now() + Duration::from_millis(100);
                let mut got = false;
                while alive && !got && Instant::now() < until {
                    match e.next_event(until - Instant::now(), &shared.stop) {
                        Some(601) => wants_input += 1,
                        Some(602) => {
                            if let Ok(Some((d, _))) = e.output() {
                                alive = out.send(&d, e.width, e.height, &shared);
                                got = true;
                            }
                        }
                        _ => break,
                    }
                }
                alive
            } else {
                fed += 1;
                let mut alive = e.input(&frame, fed) >= 0;
                while alive {
                    match e.output() {
                        Ok(Some((d, _))) => alive = out.send(&d, e.width, e.height, &shared),
                        _ => break,
                    }
                }
                alive
            }
        };
        if !ok {
            return; // writer gone: the session is over
        }
    }
}

// --------------------------------------------------------------- input ----

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
union InputUnion {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
}

#[repr(C)]
struct INPUT {
    kind: u32,
    u: InputUnion,
}

const MOUSEEVENTF_MOVE: u32 = 0x0001;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
const MOUSEEVENTF_WHEEL: u32 = 0x0800;
const MOUSEEVENTF_HWHEEL: u32 = 0x1000;
const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x1;
const KEYEVENTF_KEYUP: u32 = 0x2;
const KEYEVENTF_UNICODE: u32 = 0x4;

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
}

fn mouse(flags: u32, dx: i32, dy: i32, data: u32) -> INPUT {
    INPUT { kind: 0, u: InputUnion { mi: MOUSEINPUT { dx, dy, data, flags, time: 0, extra: 0 } } }
}

fn key(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT { kind: 1, u: InputUnion { ki: KEYBDINPUT { vk, scan, flags, time: 0, extra: 0 } } }
}

struct Input {
    shared: Arc<Shared>,
}

impl Input {
    /// Page-normalised (0..1) point → absolute SendInput coordinates
    /// (0..65535 across the virtual desktop).
    fn point(&self, x: f64, y: f64) -> (i32, i32) {
        let display = self.shared.display.lock().unwrap().clone();
        let (left, top, w, h) = crate::windows::bounds(&display).unwrap_or((0, 0, 1, 1));
        let px = left as f64 + x.clamp(0.0, 0.9999) * w as f64;
        let py = top as f64 + y.clamp(0.0, 0.9999) * h as f64;
        let (vx, vy, vw, vh) = unsafe { (GetSystemMetrics(76), GetSystemMetrics(77), GetSystemMetrics(78).max(2), GetSystemMetrics(79).max(2)) };
        (((px - vx as f64) * 65535.0 / (vw - 1) as f64).round() as i32, ((py - vy as f64) * 65535.0 / (vh - 1) as f64).round() as i32)
    }

    fn handle(&mut self, msg: &str) {
        let mut it = msg.splitn(2, ' ');
        let (cmd, rest) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        let nums: Vec<f64> = rest.split(' ').filter_map(|s| s.parse().ok()).collect();
        let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
        let abs = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        match cmd {
            "d" | "u" | "m" => {
                let (x, y) = self.point(n(0), n(1));
                let button = match (cmd, n(2) as u32) {
                    ("d", 2) => MOUSEEVENTF_RIGHTDOWN,
                    ("d", _) => MOUSEEVENTF_LEFTDOWN,
                    ("u", 2) => MOUSEEVENTF_RIGHTUP,
                    ("u", _) => MOUSEEVENTF_LEFTUP,
                    _ => 0,
                };
                // Windows derives double-clicks from timing/position itself
                send(&[mouse(MOUSEEVENTF_MOVE | abs | button, x, y, 0)]);
            }
            // natural scrolling: content follows the fingers
            "s" => {
                let (dx, dy) = (n(0), n(1));
                if dy != 0.0 {
                    send(&[mouse(MOUSEEVENTF_WHEEL, 0, 0, (dy * 3.0) as i32 as u32)]);
                }
                if dx != 0.0 {
                    send(&[mouse(MOUSEEVENTF_HWHEEL, 0, 0, (-dx * 3.0) as i32 as u32)]);
                }
            }
            "t" => {
                let mut v = vec![];
                for unit in rest.encode_utf16() {
                    v.push(key(0, unit, KEYEVENTF_UNICODE));
                    v.push(key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
                send(&v);
            }
            "k" => {
                let (vk, ext) = match rest {
                    "Enter" => (0x0D, false),
                    "Backspace" => (0x08, false),
                    "Tab" => (0x09, false),
                    "Escape" => (0x1B, false),
                    "Delete" => (0x2E, true),
                    "ArrowLeft" => (0x25, true),
                    "ArrowUp" => (0x26, true),
                    "ArrowRight" => (0x27, true),
                    "ArrowDown" => (0x28, true),
                    _ => return,
                };
                let e = if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
                send(&[key(vk, 0, e), key(vk, 0, e | KEYEVENTF_KEYUP)]);
            }
            _ => {}
        }
    }
}

// ------------------------------------------------- virtual monitor pool ----

/// Tablets on a monitor of the driver's pool: (session id, pool monitor).
static TABLETS: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Screens of tablets that just left: (client id, pool monitor, until). A
/// tablet that sleeps, reloads or hops Wi-Fi comes back to the same screen -
/// with its windows still on it - instead of a fresh one.
static KEPT: Mutex<Vec<(String, String, Instant)>> = Mutex::new(Vec::new());
const KEEP: Duration = Duration::from_secs(60);

/// Park the screens whose tablet didn't come back in time.
fn keep_reaper() {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let _tablets = TABLETS.lock().unwrap(); // no claim in between
        let mut kept = KEPT.lock().unwrap();
        let now = Instant::now();
        let mut changed = false;
        kept.retain(|(_, name, until)| {
            if *until > now {
                return true;
            }
            let _ = crate::windows::set_enabled(name, false);
            changed = true;
            false
        });
        if changed {
            reassert_off();
        }
    }
}

/// A topology change makes Windows re-apply its saved layout, which can turn
/// back on screens the user switched off with monctl: undo that.
fn reassert_off() {
    let state = crate::common::load_state();
    for d in crate::windows::list() {
        if d.active && state.contains_key(&d.id) {
            let _ = crate::windows::set_enabled(&d.id, false);
        }
    }
}

struct WinScreen {
    id: u64,
    client: String,
    shared: Arc<Shared>,
    input: Input,
    threads: Vec<std::thread::JoinHandle<()>>,
    pooled: bool,
}

impl Screen for WinScreen {
    fn display(&self) -> String {
        self.shared.display.lock().unwrap().clone()
    }
    fn request_keyframe(&self) {
        self.shared.force_key.store(true, Ordering::Relaxed);
        self.shared.slot.ready.notify_one();
    }
    fn input(&mut self, msg: &str) {
        self.input.handle(msg);
    }
}

impl Drop for WinScreen {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.slot.ready.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if self.pooled {
            let mut tablets = TABLETS.lock().unwrap();
            if let Some(i) = tablets.iter().position(|t| t.0 == self.id) {
                // stays on the desktop for a while in case the tablet comes back
                let (_, name) = tablets.remove(i);
                KEPT.lock().unwrap().push((self.client.clone(), name, Instant::now() + KEEP));
            }
        }
    }
}

// --------------------------------------------------------- platform API ----

pub(super) fn prepare(opts: &Options) {
    const PER_MONITOR_AWARE_V2: isize = -4;
    unsafe { SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2) }; // physical pixels everywhere
    // `adb devices` starts a resident adb server that inherits every
    // inheritable handle - including our redirected stdout, which then stays
    // locked (a log file can't be reopened, a tray's pipe never closes).
    // Children get explicit pipes anyway, so stop handing these down.
    unsafe {
        for std in [-10i32, -11, -12] {
            SetHandleInformation(GetStdHandle(std), 1, 0); // HANDLE_FLAG_INHERIT off
        }
        // colours for the terminal QR code (conhost needs VT processing switched on)
        let out = GetStdHandle(-11);
        let mut mode = 0u32;
        if GetConsoleMode(out, &mut mode) != 0 {
            SetConsoleMode(out, mode | 0x0004);
        }
        SetConsoleCtrlHandler(on_console_close, 1);
    }
    if opts.display.is_none() {
        // the one moment a UAC prompt may appear: installing the screen driver
        if let Err(e) = winvdd::ensure() {
            crate::common::fail(&format!("tablet screens need the virtual display driver: {e}"));
        }
        winvdd::park(); // start clean: leftovers of a crashed run
        std::thread::spawn(keep_reaper);
    }
}

pub(super) fn resolve_display(token: &str) -> Result<String, String> {
    let displays = crate::windows::list();
    let found = if token == "main" {
        displays.iter().find(|d| d.main)
    } else {
        let t = token.trim_start_matches("\\\\.\\");
        displays.iter().find(|d| d.id.trim_start_matches("\\\\.\\").eq_ignore_ascii_case(t) || d.id.ends_with(&format!("DISPLAY{t}")))
    };
    found.map(|d| d.id.clone()).ok_or_else(|| format!("no display '{token}' (see `monctl list`)"))
}

/// Sessions run on the connection's thread: no CoreGraphics-style per-process
/// cache to dodge on Windows, and the monitor pool must be shared anyway.
/// Live sessions: (session number, tablet, its socket) - to replace one.
static LIVE: Mutex<Vec<(u64, String, std::net::TcpStream)>> = Mutex::new(Vec::new());

pub(super) fn dispatch(sock: std::net::TcpStream, hello: Hello, opts: &Options) {
    // Newest connection wins: a tablet that dropped off Wi-Fi often leaves a
    // half-open connection behind; end it, and once its screen is handed
    // back (kept for this tablet), this connection takes it over.
    let old: Vec<std::net::TcpStream> = LIVE.lock().unwrap().iter().filter(|l| l.1 == hello.client).filter_map(|l| l.2.try_clone().ok()).collect();
    if !old.is_empty() {
        old.iter().for_each(super::replaced);
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until && LIVE.lock().unwrap().iter().any(|l| l.1 == hello.client) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Packet>(8);
    match open(&hello, opts, tx) {
        Ok(screen) => {
            let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            if let Ok(c) = sock.try_clone() {
                LIVE.lock().unwrap().push((n, hello.client.clone(), c));
            }
            super::TABLETS.fetch_add(1, Ordering::Relaxed);
            super::run_session(sock, &hello, screen, rx); // the screen is kept for this tablet when it returns
            super::TABLETS.fetch_sub(1, Ordering::Relaxed);
            LIVE.lock().unwrap().retain(|l| l.0 != n);
        }
        Err(e) => super::refuse(&sock, &hello, &e),
    }
}

/// Stop casting. Sessions live in this process, so their screens must be
/// handed back to the pool (off the desktop) before it ends.
pub(super) fn shutdown() -> ! {
    winvdd::park();
    reassert_off();
    std::process::exit(0)
}

extern "system" fn on_console_close(_: u32) -> i32 {
    shutdown() // Ctrl+C / closing the console window
}

pub(super) fn worker(_opts: &Options) -> ! {
    crate::common::fail("--worker is a macOS-only internal mode")
}

fn open(hello: &Hello, opts: &Options, tx: SyncSender<Packet>) -> Result<Box<dyn Screen>, String> {
    let shared = Arc::new(Shared {
        slot: Slot { frame: Mutex::new(None), ready: Condvar::new() },
        stop: AtomicBool::new(false),
        force_key: AtomicBool::new(true),
        display: Mutex::new(opts.display.clone().unwrap_or_default()),
    });
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let pooled = opts.display.is_none();
    if pooled {
        // Windows works in physical pixels: a 1180x820 @2x iPad gets 2360x1640
        let scale = if opts.hidpi { hello.dpr.max(1.0) } else { 1.0 };
        let w = ((hello.width as f64 * scale).round() as u32).clamp(640, 7680) & !1;
        let h = ((hello.height as f64 * scale).round() as u32).clamp(480, 4320) & !1;
        let mut tablets = TABLETS.lock().unwrap();
        // the driver may have crashed or restarted since we started
        if !winvdd::ping() || !winvdd::pool_ready() {
            winvdd::ensure().map_err(|e| format!("cannot add a screen: {e}"))?;
        }
        let mut kept = KEPT.lock().unwrap();
        let name = if let Some(i) = kept.iter().position(|k| k.0 == hello.client) {
            kept.remove(i).1 // this tablet's own screen, still there
        } else {
            let pool = winvdd::pool();
            let free = |kept: &Vec<(String, String, Instant)>| pool.iter().find(|m| !tablets.iter().any(|t| t.1 == **m) && !kept.iter().any(|k| k.1 == **m)).cloned();
            match free(&kept) {
                Some(n) => n,
                // pool full: the screen kept longest gives way
                None if !kept.is_empty() => {
                    let (_, n, _) = kept.remove(0);
                    let _ = crate::windows::set_enabled(&n, false);
                    n
                }
                None => return Err(format!("up to {} tablets at a time", winvdd::POOL)),
            }
        };
        drop(kept);
        let (gw, gh) = winvdd::claim(&name, w, h).map_err(|e| format!("cannot add a screen: {e}"))?;
        if (gw, gh) != (w, h) {
            eprintln!("monctl cast: {}: {w}x{h} isn't offered yet, using {gw}x{gh} (exact from the next driver start)", hello.label);
        }
        // UI at the tablet's own scale: an iPad (2x) gets 200%, not tiny 100%
        if opts.hidpi && hello.dpr > 1.0 {
            if let Err(e) = crate::windows::set_scale(&name, (hello.dpr * 100.0).round() as u32) {
                eprintln!("monctl cast: {}: {e}", hello.label);
            }
        }
        reassert_off();
        *shared.display.lock().unwrap() = name.clone();
        tablets.push((id, name));
    }
    let bitrate = opts.bitrate_mbps * 1_000_000;
    let threads = vec![
        {
            let s = shared.clone();
            std::thread::spawn(move || capture_loop(s))
        },
        {
            let s = shared.clone();
            std::thread::spawn(move || encode_loop(s, tx, bitrate))
        },
    ];
    Ok(Box::new(WinScreen { id, client: hello.client.clone(), input: Input { shared: shared.clone() }, shared, threads, pooled }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_grey() {
        // mid grey (128,128,128) → Y≈126, U=V=128 in BT.709 limited range
        let src = vec![128u8; 4 * 4 * 2];
        let out = bgra_to_nv12(&src, 16, 4, 2);
        assert!(out[..8].iter().all(|&y| (125..=127).contains(&y)));
        assert!(out[8..].iter().all(|&c| c == 128));
    }
}
