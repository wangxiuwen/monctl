// monctl cast — use any tablet (iPad, Android, Windows) as an extra screen.
//
// Zero install on the tablet: it opens a web page. Per connected tablet we
// create a virtual display sized to its screen, grab it with CGDisplayStream
// (frames only arrive when something changes), hardware-encode H.264 with
// VideoToolbox in low-latency mode and push the access units over a
// WebSocket. The page decodes with WebCodecs (secure contexts: localhost via
// USB/adb) or Media Source Extensions (plain http on the LAN), and sends
// touches back, which we replay as mouse/keyboard events.
//
// Wired: Android over USB is set up automatically with `adb reverse`.

use crate::common::fail;
use crate::virt;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PAGE: &str = include_str!("cast.html");
pub const DEFAULT_PORT: u16 = 8484;

type Ref = *mut c_void;

// ---------------------------------------------------------------- FFI ----

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: Ref;
    static kCFBooleanFalse: Ref;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    // same signatures as macos.rs declares them (rustc checks they agree)
    fn CFDictionaryCreate(a: *const c_void, k: *const *const c_void, v: *const *const c_void, n: isize, kcb: *const c_void, vcb: *const c_void) -> Ref;
    fn CFDictionaryGetValue(d: *const c_void, k: *const c_void) -> *const c_void;
    fn CFArrayGetCount(a: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
    fn CFNumberCreate(a: Ref, kind: isize, v: *const c_void) -> Ref;
    fn CFRelease(r: Ref);
}
const K_CF_NUMBER_SINT32: isize = 3;
const K_CF_NUMBER_FLOAT64: isize = 6;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayBounds(d: u32) -> CGRect;
    fn CGDisplayCopyDisplayMode(d: u32) -> Ref;
    fn CGDisplayModeGetPixelWidth(m: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(m: *const c_void) -> usize;
    fn CGDisplayModeRelease(m: Ref);
    fn CGEventCreateMouseEvent(src: Ref, kind: u32, at: CGPoint, button: u32) -> Ref;
    fn CGEventCreateKeyboardEvent(src: Ref, key: u16, down: bool) -> Ref;
    fn CGEventCreateScrollWheelEvent2(src: Ref, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> Ref;
    fn CGEventKeyboardSetUnicodeString(e: Ref, len: usize, s: *const u16);
    fn CGEventSetIntegerValueField(e: Ref, field: u32, v: i64);
    fn CGEventSetFlags(e: Ref, flags: u64);
    fn CGEventPost(tap: u32, e: Ref);
    fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: Ref) -> bool;
    static kAXTrustedCheckOptionPrompt: Ref;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    static kCMSampleAttachmentKey_NotSync: Ref;
    fn CMSampleBufferGetDataBuffer(sb: Ref) -> Ref;
    fn CMSampleBufferGetFormatDescription(sb: Ref) -> Ref;
    fn CMSampleBufferGetSampleAttachmentsArray(sb: Ref, create: bool) -> Ref;
    fn CMSampleBufferGetPresentationTimeStamp(sb: Ref) -> CMTime;
    fn CMBlockBufferGetDataLength(bb: Ref) -> usize;
    fn CMBlockBufferCopyDataBytes(bb: Ref, offset: usize, len: usize, dest: *mut u8) -> i32;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        f: Ref,
        index: usize,
        ptr: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_header_len: *mut i32,
    ) -> i32;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferCreateWithIOSurface(a: Ref, surface: Ref, attrs: Ref, out: *mut Ref) -> i32;
    fn CVPixelBufferRelease(pb: Ref);
}

type OutputCallback = extern "C" fn(refcon: *mut c_void, frame_refcon: *mut c_void, status: i32, flags: u32, sb: Ref);

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTCompressionPropertyKey_RealTime: Ref;
    static kVTCompressionPropertyKey_AllowFrameReordering: Ref;
    static kVTCompressionPropertyKey_ProfileLevel: Ref;
    static kVTCompressionPropertyKey_AverageBitRate: Ref;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: Ref;
    static kVTCompressionPropertyKey_ExpectedFrameRate: Ref;
    static kVTProfileLevel_H264_Main_AutoLevel: Ref;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: Ref;
    static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: Ref;
    fn VTCompressionSessionCreate(
        a: Ref,
        w: i32,
        h: i32,
        codec: u32,
        spec: Ref,
        src_attrs: Ref,
        data_alloc: Ref,
        cb: OutputCallback,
        refcon: *mut c_void,
        out: *mut Ref,
    ) -> i32;
    fn VTSessionSetProperty(s: Ref, key: Ref, value: Ref) -> i32;
    fn VTCompressionSessionEncodeFrame(s: Ref, img: Ref, pts: CMTime, dur: CMTime, props: Ref, frame_refcon: *mut c_void, flags_out: *mut u32) -> i32;
    fn VTCompressionSessionInvalidate(s: Ref);
}

extern "C" {
    static _NSConcreteGlobalBlock: c_void;
    fn dispatch_queue_create(label: *const c_char, attr: Ref) -> Ref;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

// CGDisplayStream is "obsoleted" in the macOS 15+ headers (ScreenCaptureKit
// is the blessed path) but the symbols still ship and work - verified on
// macOS 27. Resolve at runtime so a future removal is a clean error.
type StreamCreate = unsafe extern "C" fn(u32, usize, usize, i32, Ref, Ref, *const StreamBlock) -> Ref;
type StreamCtl = unsafe extern "C" fn(Ref) -> i32;

fn sym<T>(name: &str) -> Option<T> {
    let c = CString::new(name).unwrap();
    let p = unsafe { dlsym(RTLD_DEFAULT, c.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
}

/// Hand-built Objective-C block (Clang block ABI) for the CGDisplayStream
/// frame handler. Marked global so Block_copy keeps our pointer; `ctx` is the
/// encoder it feeds. Leaked on purpose: the stream may call it until stopped.
#[repr(C)]
struct StreamBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*const StreamBlock, i32, u64, Ref, Ref),
    descriptor: *const BlockDescriptor,
    ctx: *const Encoder,
}

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

static BLOCK_DESCRIPTOR: BlockDescriptor = BlockDescriptor { reserved: 0, size: std::mem::size_of::<StreamBlock>() };
const BLOCK_IS_GLOBAL: i32 = 1 << 28;

unsafe fn number_i32(v: i32) -> Ref {
    CFNumberCreate(std::ptr::null_mut(), K_CF_NUMBER_SINT32, &v as *const i32 as *const c_void)
}

unsafe fn number_f64(v: f64) -> Ref {
    CFNumberCreate(std::ptr::null_mut(), K_CF_NUMBER_FLOAT64, &v as *const f64 as *const c_void)
}

unsafe fn dict(pairs: &[(Ref, Ref)]) -> Ref {
    let k: Vec<*const c_void> = pairs.iter().map(|p| p.0 as *const c_void).collect();
    let v: Vec<*const c_void> = pairs.iter().map(|p| p.1 as *const c_void).collect();
    CFDictionaryCreate(std::ptr::null(), k.as_ptr(), v.as_ptr(), k.len() as isize, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
}

// ------------------------------------------------------------ encoder ----

enum Packet {
    Config { codec: String, avcc: Vec<u8>, width: u32, height: u32 },
    Frame { key: bool, ts_us: u64, data: Vec<u8> },
}

/// One capture → encode pipeline for one display.
struct Encoder {
    /// taken (dropped) on stop so the writer thread sees the channel close;
    /// the Encoder itself lives on in the leaked frame-handler block
    tx: Mutex<Option<SyncSender<Packet>>>,
    force_key: AtomicBool,
    stopped: AtomicBool,
    width: u32,
    height: u32,
    vt: Mutex<Ref>,
    last_params: Mutex<Vec<u8>>,
}

unsafe impl Send for Encoder {}
unsafe impl Sync for Encoder {}

extern "C" fn on_encoded(refcon: *mut c_void, _: *mut c_void, status: i32, _: u32, sb: Ref) {
    let enc = unsafe { &*(refcon as *const Encoder) };
    if status != 0 || sb.is_null() || enc.stopped.load(Ordering::Relaxed) {
        return;
    }
    unsafe {
        // keyframe = no NotSync attachment
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, false);
        let key = attachments.is_null()
            || CFArrayGetCount(attachments) == 0
            || CFDictionaryGetValue(CFArrayGetValueAtIndex(attachments, 0), kCMSampleAttachmentKey_NotSync).is_null();
        if key {
            if let Some((sps, pps)) = parameter_sets(CMSampleBufferGetFormatDescription(sb)) {
                let mut both = sps.clone();
                both.extend_from_slice(&pps);
                let mut last = enc.last_params.lock().unwrap();
                if *last != both {
                    *last = both;
                    let codec = format!("avc1.{:02x}{:02x}{:02x}", sps[1], sps[2], sps[3]);
                    // config must never be dropped: block until the writer takes it
                    let tx = enc.tx.lock().unwrap().clone();
                    if let Some(tx) = tx {
                        let _ = tx.send(Packet::Config { codec, avcc: avcc(&sps, &pps), width: enc.width, height: enc.height });
                    }
                }
            }
        }
        let bb = CMSampleBufferGetDataBuffer(sb);
        let len = CMBlockBufferGetDataLength(bb);
        let mut data = vec![0u8; len];
        if CMBlockBufferCopyDataBytes(bb, 0, len, data.as_mut_ptr()) != 0 {
            return;
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sb);
        let ts_us = if pts.timescale > 0 { (pts.value as i128 * 1_000_000 / pts.timescale as i128) as u64 } else { 0 };
        let tx = enc.tx.lock().unwrap().clone();
        let Some(tx) = tx else { return };
        match tx.try_send(Packet::Frame { key, ts_us, data }) {
            Ok(()) => {}
            // network can't keep up: drop this frame, and since the decoder
            // now misses a reference, restart the chain from a keyframe
            Err(TrySendError::Full(_)) => enc.force_key.store(true, Ordering::Relaxed),
            Err(TrySendError::Disconnected(_)) => enc.stopped.store(true, Ordering::Relaxed),
        }
    }
}

unsafe fn parameter_sets(fmt: Ref) -> Option<(Vec<u8>, Vec<u8>)> {
    let get = |i: usize| -> Option<Vec<u8>> {
        let (mut p, mut n, mut count, mut hl) = (std::ptr::null(), 0usize, 0usize, 0i32);
        if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, i, &mut p, &mut n, &mut count, &mut hl) != 0 || p.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(p, n).to_vec())
    };
    let sps = get(0)?;
    (sps.len() >= 4).then_some(())?;
    Some((sps, get(1)?))
}

/// AVCDecoderConfigurationRecord: what both WebCodecs (`description`) and
/// an MP4 `avcC` box want. Our access units use 4-byte NAL length prefixes.
fn avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(1);
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    v
}

unsafe extern "C" fn on_frame(block: *const StreamBlock, status: i32, time: u64, surface: Ref, _update: Ref) {
    let enc = &*(*block).ctx;
    // 0 = kCGDisplayStreamFrameStatusFrameComplete (idle/blank/stopped otherwise)
    if status != 0 || surface.is_null() || enc.stopped.load(Ordering::Relaxed) {
        return;
    }
    let vt = *enc.vt.lock().unwrap();
    if vt.is_null() {
        return;
    }
    let mut pb: Ref = std::ptr::null_mut();
    if CVPixelBufferCreateWithIOSurface(std::ptr::null_mut(), surface, std::ptr::null_mut(), &mut pb) != 0 {
        return;
    }
    let props = if enc.force_key.swap(false, Ordering::Relaxed) { dict(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, kCFBooleanTrue)]) } else { std::ptr::null_mut() };
    // display time is mach absolute time; on Apple silicon/Intel alike
    // nanosecond-ish resolution is plenty for a presentation timestamp
    let pts = CMTime { value: (time / 1000) as i64, timescale: 1_000_000, flags: 1, epoch: 0 };
    let invalid = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };
    VTCompressionSessionEncodeFrame(vt, pb, pts, invalid, props, std::ptr::null_mut(), std::ptr::null_mut());
    if !props.is_null() {
        CFRelease(props);
    }
    CVPixelBufferRelease(pb);
}

struct Capture {
    stream: Ref,
    enc: Arc<Encoder>,
}

unsafe impl Send for Capture {}

impl Capture {
    fn start(display: u32, bitrate_mbps: u32, tx: SyncSender<Packet>) -> Result<Capture, String> {
        let create: StreamCreate = sym("CGDisplayStreamCreateWithDispatchQueue").ok_or("CGDisplayStream is not available on this macOS")?;
        let start: StreamCtl = sym("CGDisplayStreamStart").ok_or("CGDisplayStreamStart missing")?;
        let (w, h) = unsafe {
            let m = CGDisplayCopyDisplayMode(display);
            if m.is_null() {
                return Err(format!("display {display} has no mode"));
            }
            let s = (CGDisplayModeGetPixelWidth(m) as u32, CGDisplayModeGetPixelHeight(m) as u32);
            CGDisplayModeRelease(m);
            s
        };
        // H.264 macroblocks: keep dimensions even
        let (w, h) = (w & !1, h & !1);
        let enc = Arc::new(Encoder {
            tx: Mutex::new(Some(tx)),
            force_key: AtomicBool::new(true),
            stopped: AtomicBool::new(false),
            width: w,
            height: h,
            vt: Mutex::new(std::ptr::null_mut()),
            last_params: Mutex::new(vec![]),
        });
        unsafe {
            let spec = dict(&[(kVTVideoEncoderSpecification_EnableLowLatencyRateControl, kCFBooleanTrue)]);
            let mut vt: Ref = std::ptr::null_mut();
            let refcon = Arc::as_ptr(&enc) as *mut c_void;
            let rc = VTCompressionSessionCreate(std::ptr::null_mut(), w as i32, h as i32, 0x6176_6331 /* avc1 */, spec, std::ptr::null_mut(), std::ptr::null_mut(), on_encoded, refcon, &mut vt);
            CFRelease(spec);
            if rc != 0 {
                return Err(format!("cannot start the H.264 encoder (VTCompressionSessionCreate {rc})"));
            }
            VTSessionSetProperty(vt, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
            VTSessionSetProperty(vt, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
            VTSessionSetProperty(vt, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Main_AutoLevel);
            let n = number_i32((bitrate_mbps * 1_000_000) as i32);
            VTSessionSetProperty(vt, kVTCompressionPropertyKey_AverageBitRate, n);
            CFRelease(n);
            // keyframes on demand (connect, drops, decoder errors), plus a slow safety net
            let n = number_i32(600);
            VTSessionSetProperty(vt, kVTCompressionPropertyKey_MaxKeyFrameInterval, n);
            CFRelease(n);
            let n = number_i32(60);
            VTSessionSetProperty(vt, kVTCompressionPropertyKey_ExpectedFrameRate, n);
            CFRelease(n);
            *enc.vt.lock().unwrap() = vt;

            let block = Box::leak(Box::new(StreamBlock {
                isa: &_NSConcreteGlobalBlock,
                flags: BLOCK_IS_GLOBAL,
                reserved: 0,
                invoke: on_frame,
                descriptor: &BLOCK_DESCRIPTOR,
                ctx: Arc::into_raw(enc.clone()), // leaked with the block, see StreamBlock
            }));
            let show_cursor: Option<*const Ref> = sym("kCGDisplayStreamShowCursor");
            let min_time: Option<*const Ref> = sym("kCGDisplayStreamMinimumFrameTime");
            let depth: Option<*const Ref> = sym("kCGDisplayStreamQueueDepth");
            let mut props = vec![];
            if let Some(k) = show_cursor {
                props.push((*k, kCFBooleanTrue));
            }
            if let Some(k) = min_time {
                props.push((*k, number_f64(1.0 / 60.0)));
            }
            if let Some(k) = depth {
                props.push((*k, number_i32(3)));
            }
            let props = dict(&props);
            let queue = dispatch_queue_create(c"monctl.cast".as_ptr(), std::ptr::null_mut());
            // '420v': the encoder's native input, no colour conversion on our side
            let stream = create(display, w as usize, h as usize, 0x3432_3076, props, queue, block);
            CFRelease(props);
            if stream.is_null() {
                return Err("cannot capture the display (Screen Recording permission? System Settings → Privacy & Security → Screen Recording)".into());
            }
            if start(stream) != 0 {
                return Err("CGDisplayStreamStart failed (Screen Recording permission?)".into());
            }
            Ok(Capture { stream, enc })
        }
    }

    fn request_keyframe(&self) {
        self.enc.force_key.store(true, Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.enc.stopped.store(true, Ordering::Relaxed);
        self.enc.tx.lock().unwrap().take();
        if let Some(stop) = sym::<StreamCtl>("CGDisplayStreamStop") {
            unsafe { stop(self.stream) };
        }
        let vt = std::mem::replace(&mut *self.enc.vt.lock().unwrap(), std::ptr::null_mut());
        unsafe {
            if !vt.is_null() {
                VTCompressionSessionInvalidate(vt);
                CFRelease(vt);
            }
            // the stream's queue may still be inside on_frame; it bails out on
            // `stopped`, but give it a moment before releasing what it points to
            let stream = self.stream as usize;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                CFRelease(stream as Ref);
            });
        }
    }
}

// -------------------------------------------------------------- input ----

const MOUSE_LEFT_DOWN: u32 = 1;
const MOUSE_LEFT_UP: u32 = 2;
const MOUSE_RIGHT_DOWN: u32 = 3;
const MOUSE_RIGHT_UP: u32 = 4;
const MOUSE_MOVED: u32 = 5;
const MOUSE_LEFT_DRAGGED: u32 = 6;
const FIELD_CLICK_STATE: u32 = 1;
const HID_TAP: u32 = 0;

struct Input {
    display: u32,
    left_down: bool,
    last_click: std::time::Instant,
    clicks: i64,
}

impl Input {
    fn point(&self, x: f64, y: f64) -> CGPoint {
        let b = unsafe { CGDisplayBounds(self.display) };
        CGPoint { x: b.x + x.clamp(0.0, 0.9999) * b.w, y: b.y + y.clamp(0.0, 0.9999) * b.h }
    }

    fn mouse(&self, kind: u32, p: CGPoint, button: u32, clicks: i64) {
        unsafe {
            let e = CGEventCreateMouseEvent(std::ptr::null_mut(), kind, p, button);
            if e.is_null() {
                return;
            }
            if clicks > 0 {
                CGEventSetIntegerValueField(e, FIELD_CLICK_STATE, clicks);
            }
            CGEventPost(HID_TAP, e);
            CFRelease(e);
        }
    }

    fn key(&self, code: u16, flags: u64) {
        unsafe {
            for down in [true, false] {
                let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, down);
                if flags != 0 {
                    CGEventSetFlags(e, flags);
                }
                CGEventPost(HID_TAP, e);
                CFRelease(e);
            }
        }
    }

    fn text(&self, s: &str) {
        for chunk in s.encode_utf16().collect::<Vec<u16>>().chunks(16) {
            unsafe {
                for down in [true, false] {
                    let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, down);
                    CGEventKeyboardSetUnicodeString(e, chunk.len(), chunk.as_ptr());
                    CGEventPost(HID_TAP, e);
                    CFRelease(e);
                }
            }
        }
    }

    /// One line from the page: "d x y b" down, "m x y" move, "u x y b" up,
    /// "s dx dy" scroll (pixels), "t text" typing, "k Name" special key.
    fn handle(&mut self, msg: &str) {
        let mut it = msg.splitn(2, ' ');
        let (cmd, rest) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        let nums: Vec<f64> = rest.split(' ').filter_map(|s| s.parse().ok()).collect();
        let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
        match cmd {
            "d" => {
                let p = self.point(n(0), n(1));
                if n(2) as u32 == 2 {
                    self.mouse(MOUSE_RIGHT_DOWN, p, 1, 1);
                    return;
                }
                // multi-click detection the way a real mouse driver does it
                self.clicks = if self.last_click.elapsed() < Duration::from_millis(400) { self.clicks + 1 } else { 1 };
                self.last_click = std::time::Instant::now();
                unsafe { CGWarpMouseCursorPosition(p) };
                self.mouse(MOUSE_LEFT_DOWN, p, 0, self.clicks);
                self.left_down = true;
            }
            "m" => {
                let p = self.point(n(0), n(1));
                self.mouse(if self.left_down { MOUSE_LEFT_DRAGGED } else { MOUSE_MOVED }, p, 0, 0);
            }
            "u" => {
                let p = self.point(n(0), n(1));
                if n(2) as u32 == 2 {
                    self.mouse(MOUSE_RIGHT_UP, p, 1, 1);
                } else {
                    self.mouse(MOUSE_LEFT_UP, p, 0, self.clicks.max(1));
                    self.left_down = false;
                }
            }
            "s" => unsafe {
                // units 0 = pixels; wheel1 vertical, wheel2 horizontal
                let e = CGEventCreateScrollWheelEvent2(std::ptr::null_mut(), 0, 2, n(1) as i32, n(0) as i32, 0);
                if !e.is_null() {
                    CGEventPost(HID_TAP, e);
                    CFRelease(e);
                }
            },
            "t" => self.text(rest),
            "k" => {
                let (code, flags) = match rest {
                    "Enter" => (36, 0),
                    "Backspace" => (51, 0),
                    "Tab" => (48, 0),
                    "Escape" => (53, 0),
                    "Delete" => (117, 0),
                    "ArrowLeft" => (123, 0),
                    "ArrowRight" => (124, 0),
                    "ArrowDown" => (125, 0),
                    "ArrowUp" => (126, 0),
                    "MissionControl" => (126, 1 << 18), // ⌃↑
                    _ => return,
                };
                self.key(code, flags);
            }
            _ => {}
        }
    }
}

fn input_allowed(prompt: bool) -> bool {
    unsafe {
        let opts = dict(&[(kAXTrustedCheckOptionPrompt, if prompt { kCFBooleanTrue } else { kCFBooleanFalse })]);
        let ok = AXIsProcessTrustedWithOptions(opts);
        CFRelease(opts);
        ok
    }
}

// ---------------------------------------------------------- websocket ----

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn ws_send(out: &mut impl Write, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut head = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => head.push(n as u8),
        n if n < 65536 => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.write_all(&head)?;
    out.write_all(payload)
}

/// Next client message: Some((opcode, payload)); None when the socket closed.
fn ws_read(r: &mut impl Read) -> Option<(u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h).ok()?;
    let opcode = h[0] & 0x0F;
    let masked = h[1] & 0x80 != 0;
    let mut len = (h[1] & 0x7F) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        r.read_exact(&mut b).ok()?;
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        r.read_exact(&mut b).ok()?;
        len = u64::from_be_bytes(b);
    }
    if len > 1 << 20 {
        return None; // the page only ever sends short control lines
    }
    let mut mask = [0u8; 4];
    if masked {
        r.read_exact(&mut mask).ok()?;
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).ok()?;
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Some((opcode, payload))
}

// -------------------------------------------------------------- server ----

pub struct Options {
    pub port: u16,
    /// stream an existing display instead of creating one per tablet
    pub display: Option<u32>,
    pub bitrate_mbps: u32,
    pub hidpi: bool,
}

/// Stable serial per tablet: macOS remembers arrangement per (vendor,
/// product, serial), so the same tablet lands in the same spot every time.
fn serial_for(client: &str) -> u32 {
    let h = sha1(client.as_bytes());
    0x1000 + (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 0xE000)
}

fn handle(mut sock: TcpStream, opts: &Options) {
    let _ = sock.set_nodelay(true);
    let mut reader = BufReader::new(match sock.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    if path.starts_with("/ws") {
        let Some(key) = headers.get("sec-websocket-key") else { return };
        let accept = base64(&sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
        let _ = write!(sock, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
        // nothing past the headers can be buffered yet: the page only talks
        // after it has seen our 101
        drop(reader);
        if let Err(e) = spawn_worker(&sock, opts) {
            eprintln!("monctl cast: cannot start a session: {e}");
        }
    } else if path == "/" || path.starts_with("/?") || path == "/index.html" {
        let _ = write!(
            sock,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{PAGE}",
            PAGE.len()
        );
    } else {
        let _ = write!(sock, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

/// One process per tablet session. A process that connected to the
/// WindowServer before a display existed never sees that display's modes
/// (its CoreGraphics cache doesn't refresh, even with a run loop), while the
/// process that *creates* a virtual display always sees it. So the worker
/// creates, captures and encodes its own screen - and when the tablet goes
/// away the process ends and the screen with it: nothing can leak.
fn spawn_worker(sock: &TcpStream, opts: &Options) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    extern "C" {
        fn dup2(from: i32, to: i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
    }
    const F_SETFD: i32 = 2;
    let fd = sock.as_raw_fd();
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(["cast", "--worker", "--bitrate", &opts.bitrate_mbps.to_string()]);
    if let Some(d) = opts.display {
        cmd.args(["--display", &d.to_string()]);
    }
    if !opts.hidpi {
        cmd.arg("--no-hidpi");
    }
    // the session socket becomes the worker's fd 3 (dup2 clears close-on-exec;
    // if it already is 3, clear the flag by hand)
    unsafe {
        cmd.pre_exec(move || {
            let r = if fd == 3 { fcntl(3, F_SETFD, 0) } else { dup2(fd, 3) };
            if r < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait()); // reap it
    Ok(())
}

fn session(sock: TcpStream, opts: &Options) {
    let _ = sock.set_nodelay(true);
    let mut reader = match sock.try_clone() {
        Ok(s) => BufReader::new(s),
        Err(_) => return,
    };
    let peer = sock.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    // first message: "h <css width> <css height> <devicePixelRatio> <client id> <label...>"
    let hello = match ws_read(&mut reader) {
        Some((1, p)) => String::from_utf8_lossy(&p).to_string(),
        _ => return,
    };
    let f: Vec<&str> = hello.splitn(6, ' ').collect();
    if f.first() != Some(&"h") || f.len() < 5 {
        return;
    }
    let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let (cw, ch, dpr) = (num(1).round() as u32, num(2).round() as u32, num(3));
    let client = f[4].to_string();
    let label = f.get(5).copied().unwrap_or("tablet").to_string();
    let fail_session = |e: &str| {
        eprintln!("monctl cast: {label}: {e}");
        let mut s = &sock;
        let _ = ws_send(&mut s, 1, format!("err {e}").as_bytes());
    };

    // stream an existing display, or create one shaped like the tablet
    let mut owned = None;
    let display = match opts.display {
        Some(d) => d,
        None => {
            let hidpi = opts.hidpi && dpr >= 1.5;
            let spec = virt::Spec { width: cw.clamp(640, 4096) & !1, height: ch.clamp(480, 4096) & !1, hz: 60, hidpi };
            match unsafe { virt::create(&spec, serial_for(&client), &label) } {
                Ok((d, id)) => {
                    owned = Some(d);
                    id
                }
                Err(e) => return fail_session(&format!("cannot create a screen: {e}")),
            }
        }
    };
    println!("+ {label} ({peer}, {cw}x{ch} @{dpr}x) -> display {display}");

    let (tx, rx) = sync_channel::<Packet>(8);
    let capture = match Capture::start(display, opts.bitrate_mbps, tx) {
        Ok(c) => c,
        Err(e) => return fail_session(&e),
    };
    let writer = {
        let mut out = match sock.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        };
        std::thread::spawn(move || writer_loop(&mut out, rx))
    };

    let mut input = Input { display, left_down: false, last_click: std::time::Instant::now() - Duration::from_secs(1), clicks: 0 };
    let can_input = input_allowed(false);
    while let Some((op, payload)) = ws_read(&mut reader) {
        match op {
            1 => {
                let msg = String::from_utf8_lossy(&payload);
                if msg == "key" {
                    capture.request_keyframe();
                } else if can_input {
                    input.handle(&msg);
                }
            }
            8 => break,
            9 => {
                let mut s = &sock;
                let _ = ws_send(&mut s, 10, &payload);
            }
            _ => {}
        }
    }
    drop(capture); // stops capture; the writer sees the channel close
    let _ = writer.join();
    let _ = sock.shutdown(std::net::Shutdown::Both);
    if let Some(d) = owned {
        unsafe { virt::release(d) };
    }
    println!("- {label} disconnected");
}

fn writer_loop(out: &mut TcpStream, rx: Receiver<Packet>) {
    for p in rx {
        let r = match p {
            Packet::Config { codec, avcc, width, height } => ws_send(out, 1, format!("cfg {codec} {width} {height} {}", base64(&avcc)).as_bytes()),
            Packet::Frame { key, ts_us, data } => {
                let mut buf = Vec::with_capacity(data.len() + 9);
                buf.push(key as u8);
                buf.extend_from_slice(&ts_us.to_be_bytes());
                buf.extend_from_slice(&data);
                ws_send(out, 2, &buf)
            }
        };
        if r.is_err() {
            break;
        }
    }
}

// ---------------------------------------------------- connection help ----

/// LAN IPv4 addresses worth printing (no loopback / link-local).
fn lan_addresses() -> Vec<String> {
    #[repr(C)]
    struct IfAddrs {
        next: *mut IfAddrs,
        name: *const c_char,
        flags: u32,
        addr: *const SockAddrIn,
        netmask: *const c_void,
        dst: *const c_void,
        data: *const c_void,
    }
    #[repr(C)]
    struct SockAddrIn {
        len: u8,
        family: u8,
        port: u16,
        addr: [u8; 4],
    }
    extern "C" {
        fn getifaddrs(out: *mut *mut IfAddrs) -> i32;
        fn freeifaddrs(p: *mut IfAddrs);
    }
    let mut out = vec![];
    unsafe {
        let mut head: *mut IfAddrs = std::ptr::null_mut();
        if getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let a = (*p).addr;
            if !a.is_null() && (*a).family == 2 {
                let ip = (*a).addr;
                let name = std::ffi::CStr::from_ptr((*p).name).to_string_lossy();
                // skip loopback, link-local and VPN tunnels (utun) nobody can reach
                if ip[0] != 127 && !(ip[0] == 169 && ip[1] == 254) && !name.starts_with("utun") {
                    let s = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
            p = (*p).next;
        }
        freeifaddrs(head);
    }
    out
}

/// Android over USB: tunnel the port with `adb reverse` and open the page on
/// the tablet - plug in, run `monctl cast`, done. localhost is a secure
/// context, so the tablet also gets the lowest-latency (WebCodecs) decoder.
fn wire_android(port: u16) {
    let Ok(out) = std::process::Command::new("adb").arg("devices").output() else { return };
    let serials: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|l| l.strip_suffix("\tdevice").map(str::to_string))
        .collect();
    for s in serials {
        let tcp = format!("tcp:{port}");
        let ok = std::process::Command::new("adb").args(["-s", &s, "reverse", &tcp, &tcp]).output().is_ok_and(|o| o.status.success());
        if ok {
            let url = format!("http://localhost:{port}/");
            let _ = std::process::Command::new("adb")
                .args(["-s", &s, "shell", "am", "start", "-a", "android.intent.action.VIEW", "-d", &url])
                .output();
            println!("USB  Android {s}: wired via adb, opened {url} on the tablet");
        }
    }
}

pub fn serve(opts: Options) -> ! {
    let listener = TcpListener::bind(("0.0.0.0", opts.port)).unwrap_or_else(|e| fail(&format!("cannot listen on port {}: {e}", opts.port)));
    if !input_allowed(true) {
        println!("note: touch input needs Accessibility permission for this terminal (System Settings → Privacy & Security → Accessibility); showing the screen works without it");
    }
    let addrs = lan_addresses();
    println!("monctl cast - open on the tablet (same Wi-Fi):");
    for a in &addrs {
        println!("  http://{a}:{}/", opts.port);
    }
    if let Some(a) = addrs.first() {
        print!("{}", crate::qr::terminal(&format!("http://{a}:{}/", opts.port)));
    }
    wire_android(opts.port);
    println!("each tablet gets its own screen; Ctrl+C stops casting");
    let opts = Arc::new(opts);
    for sock in listener.incoming().flatten() {
        let o = opts.clone();
        std::thread::spawn(move || handle(sock, &o));
    }
    std::process::exit(0)
}

pub fn run(args: &[String]) -> ! {
    let mut opts = Options { port: DEFAULT_PORT, display: None, bitrate_mbps: 12, hidpi: true };
    let worker = args.first().map(|s| s.as_str()) == Some("--worker");
    let mut it = args.iter().skip(worker as usize);
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| fail(&format!("{a} needs a value")));
        match a.as_str() {
            "--port" | "-p" => opts.port = val().parse().unwrap_or_else(|_| fail("bad --port")),
            "--display" | "--mirror" => {
                let v = val();
                opts.display = Some(if v == "main" { crate::macos::main_id().parse().unwrap() } else { v.parse().unwrap_or_else(|_| fail("bad --display id")) });
            }
            "--bitrate" => opts.bitrate_mbps = val().trim_end_matches("M").parse().unwrap_or_else(|_| fail("bad --bitrate (Mbit/s)")),
            "--no-hidpi" | "--lodpi" => opts.hidpi = false,
            _ => fail("usage: monctl cast [--port N] [--display ID|main] [--bitrate MBPS] [--no-hidpi]"),
        }
    }
    if worker {
        use std::os::fd::FromRawFd;
        // stopping `monctl cast` (Ctrl+C, the tray's "stop") must take every
        // tablet screen with it: a worker whose server is gone ends itself
        extern "C" {
            fn getppid() -> i32;
        }
        let parent = unsafe { getppid() };
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            if unsafe { getppid() } != parent {
                std::process::exit(0);
            }
        });
        session(unsafe { TcpStream::from_raw_fd(3) }, &opts);
        std::process::exit(0);
    }
    serve(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_and_base64_match_the_websocket_rfc_example() {
        // RFC 6455 section 1.3
        let accept = base64(&sha1(b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"));
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
    }

    #[test]
    fn avcc_record() {
        let sps = [0x67, 0x4d, 0x00, 0x28, 0xaa];
        let pps = [0x68, 0xee];
        let r = avcc(&sps, &pps);
        assert_eq!(&r[..6], &[1, 0x4d, 0x00, 0x28, 0xFF, 0xE1]);
        assert_eq!(&r[6..8], &[0, 5]);
        assert_eq!(&r[13..16], &[1, 0, 2]);
    }
}
