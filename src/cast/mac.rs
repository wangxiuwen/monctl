// macOS side of `monctl cast`: one worker process per tablet creates a
// virtual display shaped like it (CGVirtualDisplay), captures it with
// CGDisplayStream (frames only when something changes), hardware-encodes
// H.264 with VideoToolbox in low-latency mode, and replays touches as
// CGEvents.

use super::{avcc, sha1, Hello, Options, Packet, Screen};
use crate::virt;
use std::ffi::{c_char, c_void, CString};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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


// --------------------------------------------------------- platform API ----

pub(super) fn prepare(_opts: &Options) {
    if !input_allowed(true) {
        println!("note: touch input needs Accessibility permission for this terminal (System Settings → Privacy & Security → Accessibility); showing the screen works without it");
    }
}

pub(super) fn resolve_display(token: &str) -> Result<String, String> {
    if token == "main" {
        return Ok(crate::macos::main_id());
    }
    token.parse::<u32>().map(|d| d.to_string()).map_err(|_| format!("bad --display id '{token}'"))
}

/// The server hands each tablet's socket to its own worker process.
pub(super) fn dispatch(sock: TcpStream, opts: &Options) {
    if let Err(e) = spawn_worker(&sock, opts) {
        eprintln!("monctl cast: cannot start a session: {e}");
    }
}

/// `monctl cast --worker`: run one session on the socket handed over as fd 3.
pub(super) fn worker(opts: &Options) -> ! {
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
    super::session(unsafe { TcpStream::from_raw_fd(3) }, opts);
    std::process::exit(0)
}

fn serial_for(client: &str) -> u32 {
    let h = sha1(client.as_bytes());
    0x1000 + (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 0xE000)
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
    if let Some(d) = &opts.display {
        cmd.args(["--display", d]);
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


struct MacScreen {
    capture: Option<Capture>,
    input: Input,
    can_input: bool,
    display: u32,
    owned: Option<virt::Id>,
}

impl Screen for MacScreen {
    fn display(&self) -> String {
        self.display.to_string()
    }
    fn request_keyframe(&self) {
        if let Some(c) = &self.capture {
            c.request_keyframe();
        }
    }
    fn input(&mut self, msg: &str) {
        if self.can_input {
            self.input.handle(msg);
        }
    }
}

impl Drop for MacScreen {
    fn drop(&mut self) {
        self.capture.take(); // stop capturing before the display goes away
        if let Some(d) = self.owned.take() {
            unsafe { virt::release(d) };
        }
    }
}

/// Stream an existing display, or create one shaped like the tablet.
pub(super) fn open(hello: &Hello, opts: &Options, tx: SyncSender<Packet>) -> Result<Box<dyn Screen>, String> {
    let mut owned = None;
    let display = match &opts.display {
        Some(d) => d.parse().map_err(|_| format!("bad display id {d}"))?,
        None => {
            let hidpi = opts.hidpi && hello.dpr >= 1.5;
            let spec = virt::Spec { width: hello.width.clamp(640, 4096) & !1, height: hello.height.clamp(480, 4096) & !1, hz: 60, hidpi };
            let (d, id) = unsafe { virt::create(&spec, serial_for(&hello.client), &hello.label) }.map_err(|e| format!("cannot create a screen: {e}"))?;
            owned = Some(d);
            id
        }
    };
    let mut screen = MacScreen {
        capture: None,
        input: Input { display, left_down: false, last_click: std::time::Instant::now() - Duration::from_secs(1), clicks: 0 },
        can_input: input_allowed(false),
        display,
        owned,
    };
    screen.capture = Some(Capture::start(display, opts.bitrate_mbps, tx)?); // on error the drop releases the display
    Ok(Box::new(screen))
}
