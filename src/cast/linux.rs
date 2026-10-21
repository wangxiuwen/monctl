// Linux (X11) side of `monctl cast`. A tablet's screen is a RandR monitor
// carved out of an enlarged framebuffer (`xrandr --fb` + `--setmonitor`):
// window managers treat it like a real screen. ffmpeg (x11grab + libx264,
// zero-latency) captures and encodes that region; its Annex-B output goes the
// same way as the Windows encoder's. Touch input goes through XTest (libXtst,
// loaded at runtime - no build-time X dependency); typing uses xdotool.
//
// X11 has no per-monitor scaling, so a tablet's screen is its CSS size: UI at
// a normal size, slightly soft on a high-density tablet.
//
// Runtime tools: xrandr, ffmpeg (with libx264), xdotool (typing only).

use super::{annexb_to_avcc, avcc, Hello, Options, Packet, Screen};
use crate::common::fail;
use std::ffi::{c_char, c_void, CString};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program).args(args).output().map_err(|e| format!("{program}: {e} (is it installed?)"))?;
    if !out.status.success() {
        return Err(format!("{program} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------- screens (RandR) ----

/// A tablet's area of the X screen.
#[derive(Clone, Debug, PartialEq)]
struct Region {
    name: String,
    x: u32,
    w: u32,
    h: u32,
    /// the spare video output driving it (pointer can reach it); None = a
    /// bare RandR monitor (viewing only: X confines the pointer to CRTCs)
    output: Option<String>,
}

/// RandR monitors: (name, x, y, w, h) from `xrandr --listmonitors`
/// ("0: +*HDMI-1 1920/527x1080/296+0+0  HDMI-1").
fn monitors() -> Vec<(String, u32, u32, u32, u32)> {
    let out = run("xrandr", &["--listmonitors"]).unwrap_or_default();
    out.lines().skip(1).filter_map(parse_monitor).collect()
}

fn parse_monitor(line: &str) -> Option<(String, u32, u32, u32, u32)> {
    let mut t = line.split_whitespace();
    let name = t.nth(1)?.trim_start_matches(['+', '*']).to_string();
    let geo = t.next()?; // 1920/527x1080/296+0+0
    let (size, pos) = geo.split_once('+')?;
    let (w, h) = size.split_once('x')?;
    let w = w.split('/').next()?.parse().ok()?;
    let h = h.split('/').next()?.parse().ok()?;
    let (x, y) = pos.split_once('+')?;
    Some((name, x.parse().ok()?, y.parse().ok()?, w, h))
}

/// Framebuffer size from `xrandr --query`'s first line ("current 1920 x 1080").
fn framebuffer() -> (u32, u32) {
    let out = run("xrandr", &["--query"]).unwrap_or_default();
    let first = out.lines().next().unwrap_or("");
    let cur = first.split("current ").nth(1).unwrap_or("");
    let mut t = cur.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
    (t.next().and_then(|s| s.parse().ok()).unwrap_or(0), t.next().and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// CVT reduced-blanking modeline for w x h at 60 Hz (what `cvt -r` prints):
/// (pixel clock MHz, h sync start, h sync end, h total, v sync start, v sync end, v total)
fn cvt_rb(w: u32, h: u32) -> (f64, u32, u32, u32, u32, u32, u32) {
    let htotal = w + 160;
    let hss = w + 48;
    let hse = w + 80;
    // vertical blanking of at least 460 us
    let line_us = (1e6 / 60.0 - 460.0) / h as f64;
    let vblank = ((460.0 / line_us).ceil() as u32).max(3 + 10 + 6);
    let ratio = w as f64 / h as f64;
    let vsync = [(4.0 / 3.0, 4), (16.0 / 9.0, 5), (16.0 / 10.0, 6), (5.0 / 4.0, 7)].iter().find(|(r, _)| (ratio - r).abs() < 0.01).map_or(10, |v| v.1);
    let vtotal = h + vblank;
    // pixel clock in 0.25 MHz steps
    let clock = ((htotal as f64 * vtotal as f64 * 60.0 / 1e6) * 4.0).floor() / 4.0;
    (clock, hss, hse, htotal, h + 3, h + 3 + vsync, vtotal)
}

/// Outputs with nothing on them (and not ours yet), VIRTUAL* first.
fn spare_outputs() -> Vec<String> {
    let out = run("xrandr", &["--query"]).unwrap_or_default();
    let mut v: Vec<String> = out
        .lines()
        .filter(|l| !l.starts_with(' ') && (l.contains(" disconnected") || l.contains(" connected")))
        .filter(|l| !l.split_whitespace().any(|t| t.contains('x') && t.contains('+'))) // no geometry = unused
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    v.sort_by_key(|n| !n.starts_with("VIRTUAL"));
    v
}

/// x position of an active output (`OUT connected 1280x800+1920+0`).
fn output_x(name: &str) -> Option<u32> {
    let out = run("xrandr", &["--query"]).ok()?;
    let line = out.lines().find(|l| l.split_whitespace().next() == Some(name))?;
    let geo = line.split_whitespace().find(|t| t.contains('x') && t.contains('+'))?;
    geo.split('+').nth(1)?.parse().ok()
}

/// A new screen right of the desktop: on a spare video output with a mode of
/// exactly the tablet's size when there is one (a real CRTC: the pointer -
/// so touch - can reach it), else a bare RandR monitor.
fn add_region(name: &str, w: u32, h: u32) -> Result<Region, String> {
    let right_of = monitors().into_iter().max_by_key(|m| m.1 + m.3);
    let mode = format!("monctl-{w}x{h}");
    let (clock, hss, hse, ht, vss, vse, vt) = cvt_rb(w, h);
    let line = [format!("{clock:.2}"), w.to_string(), hss.to_string(), hse.to_string(), ht.to_string(), h.to_string(), vss.to_string(), vse.to_string(), vt.to_string()];
    let mut args = vec!["--newmode", &mode];
    args.extend(line.iter().map(String::as_str));
    args.extend(["+hsync", "-vsync"]);
    let _ = run("xrandr", &args); // already defined: fine
    let taken: Vec<String> = REGIONS.lock().unwrap().iter().filter_map(|r| r.output.clone()).collect();
    for out in spare_outputs().into_iter().filter(|o| !taken.contains(o)) {
        if run("xrandr", &["--addmode", &out, &mode]).is_err() {
            continue;
        }
        let mut cmd = vec!["--output", &out, "--mode", &mode];
        let anchor = right_of.as_ref().map(|m| m.0.clone()).unwrap_or_default();
        if !anchor.is_empty() {
            cmd.extend(["--right-of", &anchor]);
        }
        if run("xrandr", &cmd).is_ok() {
            if let Some(x) = output_x(&out) {
                return Ok(Region { name: name.to_string(), x, w, h, output: Some(out) });
            }
        }
        let _ = run("xrandr", &["--delmode", &out, &mode]);
    }
    // no spare output: grow the framebuffer and declare a monitor there
    let right = right_of.map(|m| m.1 + m.3).unwrap_or(0);
    let (fw, fh) = framebuffer();
    let (nw, nh) = ((right + w).max(fw), h.max(fh));
    if (nw, nh) != (fw, fh) {
        run("xrandr", &["--fb", &format!("{nw}x{nh}")])?;
    }
    let mm = |px: u32| px * 254 / 1100; // ~110 ppi, like a desktop monitor
    run("xrandr", &["--setmonitor", name, &format!("{w}/{}x{h}/{}+{right}+0", mm(w), mm(h)), "none"])?;
    eprintln!("monctl cast: no spare video output - the tablet shows its screen, but X won't let the pointer (touch) onto it");
    Ok(Region { name: name.to_string(), x: right, w, h, output: None })
}

/// Remove a tablet's screen and give its area back.
fn remove_region(r: &Region) {
    match &r.output {
        Some(out) => {
            let mode = format!("monctl-{}x{}", r.w, r.h);
            let _ = run("xrandr", &["--output", out, "--off"]);
            let _ = run("xrandr", &["--delmode", out, &mode]);
        }
        None => {
            let _ = run("xrandr", &["--delmonitor", &r.name]);
        }
    }
    let rest: Vec<_> = monitors();
    let right = rest.iter().map(|m| m.1 + m.3).max().unwrap_or(0);
    let bottom = rest.iter().map(|m| m.2 + m.4).max().unwrap_or(0);
    if right > 0 && bottom > 0 {
        let _ = run("xrandr", &["--fb", &format!("{right}x{bottom}")]);
    }
}

// ------------------------------------------------- capture + encoding ----

/// ffmpeg grabbing a region and writing H.264 (Annex-B, an AUD before every
/// access unit, SPS/PPS with every keyframe) to stdout.
struct Encoder {
    child: Child,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Encoder {
    fn start(r: &Region, bitrate_mbps: u32, tx: SyncSender<Packet>, stop: Arc<AtomicBool>) -> Result<Encoder, String> {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
        let rate = format!("{bitrate_mbps}M");
        let mut child = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-f", "x11grab", "-draw_mouse", "1", "-framerate", "30"])
            .args(["-video_size", &format!("{}x{}", r.w, r.h), "-i", &format!("{display}+{},0", r.x)])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-profile:v", "main", "-pix_fmt", "yuv420p"])
            .args(["-g", "30", "-bf", "0", "-b:v", &rate, "-maxrate", &rate, "-bufsize", &format!("{}M", bitrate_mbps.div_ceil(2))])
            .args(["-x264-params", "aud=1:repeat-headers=1", "-f", "h264", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e} (install it: sudo apt install ffmpeg)"))?;
        let mut out = child.stdout.take().unwrap();
        let (w, h) = (r.w, r.h);
        let reader = std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            let mut pending: Vec<u8> = vec![];
            let mut params: Vec<u8> = vec![];
            let mut frames = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let n = match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buf[..n]);
                // complete access units: everything before the last AUD
                while let Some(cut) = next_aud(&pending, 4) {
                    let au: Vec<u8> = pending.drain(..cut).collect();
                    let (data, ps, key) = annexb_to_avcc(&au);
                    if let Some((sps, pps)) = ps {
                        let both = [sps.as_slice(), pps.as_slice()].concat();
                        if both != params && sps.len() >= 4 {
                            params = both;
                            let codec = format!("avc1.{:02x}{:02x}{:02x}", sps[1], sps[2], sps[3]);
                            if tx.send(Packet::Config { codec, avcc: avcc(&sps, &pps), width: w, height: h }).is_err() {
                                return;
                            }
                        }
                    }
                    if data.is_empty() {
                        continue;
                    }
                    frames += 1;
                    match tx.try_send(Packet::Frame { key, ts_us: frames * 33_333, data }) {
                        Ok(()) | Err(TrySendError::Full(_)) => {} // dropped frames heal at the next keyframe (1 s)
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
            }
        });
        Ok(Encoder { child, reader: Some(reader) })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

/// Offset of the first access unit delimiter (00 00 00 01 09 or 00 00 01 09)
/// at or after `from`.
fn next_aud(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 4 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 {
            if b[i + 2] == 1 && b[i + 3] & 0x1f == 9 {
                return Some(i);
            }
            if b[i + 2] == 0 && i + 4 < b.len() && b[i + 3] == 1 && b[i + 4] & 0x1f == 9 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

// --------------------------------------------------------------- input ----

/// XTest through libX11/libXtst, loaded at runtime.
struct XTest {
    dpy: *mut c_void,
    motion: unsafe extern "C" fn(*mut c_void, i32, i32, i32, u64) -> i32,
    button: unsafe extern "C" fn(*mut c_void, u32, i32, u64) -> i32,
    key: unsafe extern "C" fn(*mut c_void, u32, i32, u64) -> i32,
    keycode: unsafe extern "C" fn(*mut c_void, u64) -> u8,
    flush: unsafe extern "C" fn(*mut c_void) -> i32,
}

unsafe impl Send for XTest {}

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

impl XTest {
    fn open() -> Option<XTest> {
        unsafe {
            let x11 = dlopen(c"libX11.so.6".as_ptr(), 2);
            let xtst = dlopen(c"libXtst.so.6".as_ptr(), 2);
            if x11.is_null() || xtst.is_null() {
                return None;
            }
            let get = |lib: *mut c_void, name: &str| {
                let c = CString::new(name).unwrap();
                dlsym(lib, c.as_ptr())
            };
            let open: unsafe extern "C" fn(*const c_char) -> *mut c_void = std::mem::transmute(get(x11, "XOpenDisplay").as_ref()?);
            let dpy = open(std::ptr::null());
            if dpy.is_null() {
                return None;
            }
            Some(XTest {
                dpy,
                motion: std::mem::transmute(get(xtst, "XTestFakeMotionEvent").as_ref()?),
                button: std::mem::transmute(get(xtst, "XTestFakeButtonEvent").as_ref()?),
                key: std::mem::transmute(get(xtst, "XTestFakeKeyEvent").as_ref()?),
                keycode: std::mem::transmute(get(x11, "XKeysymToKeycode").as_ref()?),
                flush: std::mem::transmute(get(x11, "XFlush").as_ref()?),
            })
        }
    }
}

struct Input {
    x: Option<XTest>,
    region: Arc<Mutex<Region>>,
    scroll: (f64, f64),
}

impl Input {
    fn point(&self, x: f64, y: f64) -> (i32, i32) {
        let r = self.region.lock().unwrap();
        ((r.x as f64 + x.clamp(0.0, 0.9999) * r.w as f64) as i32, (y.clamp(0.0, 0.9999) * r.h as f64) as i32)
    }

    fn handle(&mut self, msg: &str) {
        let mut it = msg.splitn(2, ' ');
        let (cmd, rest) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if cmd == "t" {
            // text: xdotool knows the keyboard layout and every character
            let _ = Command::new("xdotool").args(["type", "--delay", "0", "--", rest]).status();
            return;
        }
        let Some(x) = &self.x else { return };
        let nums: Vec<f64> = rest.split(' ').filter_map(|s| s.parse().ok()).collect();
        let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
        unsafe {
            match cmd {
                "d" | "u" | "m" => {
                    let (px, py) = self.point(n(0), n(1));
                    (x.motion)(x.dpy, -1, px, py, 0);
                    if cmd != "m" {
                        let button = if n(2) as u32 == 2 { 3 } else { 1 };
                        (x.button)(x.dpy, button, (cmd == "d") as i32, 0);
                    }
                }
                "s" => {
                    // natural scrolling in wheel clicks (4 up, 5 down, 6 left, 7 right)
                    self.scroll.0 += n(0);
                    self.scroll.1 += n(1);
                    const STEP: f64 = 20.0;
                    while self.scroll.1.abs() >= STEP {
                        let b = if self.scroll.1 > 0.0 { 4 } else { 5 };
                        (x.button)(x.dpy, b, 1, 0);
                        (x.button)(x.dpy, b, 0, 0);
                        self.scroll.1 -= STEP.copysign(self.scroll.1);
                    }
                    while self.scroll.0.abs() >= STEP {
                        let b = if self.scroll.0 > 0.0 { 6 } else { 7 };
                        (x.button)(x.dpy, b, 1, 0);
                        (x.button)(x.dpy, b, 0, 0);
                        self.scroll.0 -= STEP.copysign(self.scroll.0);
                    }
                }
                "k" => {
                    let keysym: u64 = match rest {
                        "Enter" => 0xff0d,
                        "Backspace" => 0xff08,
                        "Tab" => 0xff09,
                        "Escape" => 0xff1b,
                        "Delete" => 0xffff,
                        "ArrowLeft" => 0xff51,
                        "ArrowUp" => 0xff52,
                        "ArrowRight" => 0xff53,
                        "ArrowDown" => 0xff54,
                        _ => return,
                    };
                    let code = (x.keycode)(x.dpy, keysym) as u32;
                    (x.key)(x.dpy, code, 1, 0);
                    (x.key)(x.dpy, code, 0, 0);
                }
                _ => {}
            }
            (x.flush)(x.dpy);
        }
    }
}

// ---------------------------------------------------------- sessions ----

static NEXT: AtomicU32 = AtomicU32::new(1);
/// Live sessions: (number, tablet, socket) - to replace one (newest wins).
static LIVE: Mutex<Vec<(u32, String, std::net::TcpStream)>> = Mutex::new(Vec::new());
/// Screens of tablets that just left: (tablet, region, until).
static KEPT: Mutex<Vec<(String, Region, Instant)>> = Mutex::new(Vec::new());
/// Every region we made, for shutdown.
static REGIONS: Mutex<Vec<Region>> = Mutex::new(Vec::new());
const KEEP: Duration = Duration::from_secs(60);

fn keep_reaper() {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let now = Instant::now();
        let expired: Vec<Region> = {
            let mut kept = KEPT.lock().unwrap();
            let (gone, stay): (Vec<_>, Vec<_>) = kept.drain(..).partition(|k| k.2 <= now);
            *kept = stay;
            gone.into_iter().map(|k| k.1).collect()
        };
        for r in expired {
            REGIONS.lock().unwrap().retain(|x| x != &r);
            remove_region(&r);
        }
    }
}

struct LinuxScreen {
    client: String,
    region: Arc<Mutex<Region>>,
    encoder: Option<Encoder>,
    stop: Arc<AtomicBool>,
    input: Input,
}

impl Screen for LinuxScreen {
    fn display(&self) -> String {
        self.region.lock().unwrap().name.clone()
    }
    fn request_keyframe(&self) {} // ffmpeg sends one every second anyway
    fn input(&mut self, msg: &str) {
        self.input.handle(msg);
    }
}

impl Drop for LinuxScreen {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.encoder.take();
        // kept for a while in case the tablet comes back
        let r = self.region.lock().unwrap().clone();
        KEPT.lock().unwrap().push((self.client.clone(), r, Instant::now() + KEEP));
    }
}

fn open(hello: &Hello, opts: &Options, tx: SyncSender<Packet>) -> Result<Box<dyn Screen>, String> {
    let (w, h) = (hello.width.clamp(640, 4096) & !1, hello.height.clamp(480, 4096) & !1);
    // its own screen if it's back (resized on rotation), else a new one
    let mine = {
        let mut kept = KEPT.lock().unwrap();
        kept.iter().position(|k| k.0 == hello.client).map(|i| kept.remove(i).1)
    };
    let region = match mine {
        Some(r) if (r.w, r.h) == (w, h) => r,
        other => {
            if let Some(r) = other {
                REGIONS.lock().unwrap().retain(|x| x != &r);
                remove_region(&r);
            }
            let name = format!("monctl-{}", NEXT.fetch_add(1, Ordering::Relaxed));
            let r = add_region(&name, w, h).map_err(|e| format!("cannot add a screen: {e}"))?;
            REGIONS.lock().unwrap().push(r.clone());
            r
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let encoder = Encoder::start(&region, opts.bitrate_mbps, tx, stop.clone())?;
    let region = Arc::new(Mutex::new(region));
    Ok(Box::new(LinuxScreen {
        client: hello.client.clone(),
        input: Input { x: XTest::open(), region: region.clone(), scroll: (0.0, 0.0) },
        region,
        encoder: Some(encoder),
        stop,
    }))
}

// --------------------------------------------------------- platform API ----

pub(super) fn prepare(opts: &Options) {
    if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland") || std::env::var_os("DISPLAY").is_none() {
        fail("casting needs an X11 session (Wayland can't add screens from outside the compositor) - log in with \"GNOME on Xorg\" / \"Plasma (X11)\"");
    }
    if opts.display.is_some() {
        fail("--display isn't supported on Linux yet");
    }
    for (tool, hint) in [("xrandr", "x11-xserver-utils"), ("ffmpeg", "ffmpeg")] {
        if Command::new(tool).arg("-version").output().is_err() && Command::new(tool).arg("--version").output().is_err() {
            fail(&format!("casting needs {tool}: sudo apt install {hint}"));
        }
    }
    if XTest::open().is_none() {
        println!("note: touch input needs libXtst (sudo apt install libxtst6); showing the screen works without it");
    }
    // leftovers of a crashed run: bare monitors and outputs on our modes
    for m in monitors().into_iter().filter(|m| m.0.starts_with("monctl-")) {
        let _ = run("xrandr", &["--delmonitor", &m.0]);
    }
    let query = run("xrandr", &["--query"]).unwrap_or_default();
    let mut current = None;
    for line in query.lines() {
        if !line.starts_with(' ') {
            current = line.split_whitespace().next().map(str::to_string);
        } else if line.trim_start().starts_with("monctl-") && line.contains('*') {
            if let Some(out) = &current {
                let _ = run("xrandr", &["--output", out, "--off"]);
            }
        }
    }
    std::thread::spawn(keep_reaper);
}

pub(super) fn resolve_display(_token: &str) -> Result<String, String> {
    Err("--display isn't supported on Linux yet".into())
}

pub(super) fn dispatch(sock: std::net::TcpStream, hello: Hello, opts: &Options) {
    // newest connection wins (a Wi-Fi drop leaves a half-open one behind)
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
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            if let Ok(c) = sock.try_clone() {
                LIVE.lock().unwrap().push((n, hello.client.clone(), c));
            }
            super::TABLETS.fetch_add(1, Ordering::Relaxed);
            super::run_session(sock, &hello, screen, rx);
            super::TABLETS.fetch_sub(1, Ordering::Relaxed);
            LIVE.lock().unwrap().retain(|l| l.0 != n);
        }
        Err(e) => super::refuse(&sock, &hello, &e),
    }
}

pub(super) fn shutdown() -> ! {
    for r in REGIONS.lock().unwrap().drain(..) {
        remove_region(&r);
    }
    std::process::exit(0)
}

pub(super) fn worker(_opts: &Options) -> ! {
    fail("--worker is a macOS-only internal mode")
}

#[cfg(test)]
mod tests {
    #[test]
    fn listmonitors_line() {
        assert_eq!(super::parse_monitor(" 0: +*HDMI-1 1920/527x1080/296+0+0  HDMI-1"), Some(("HDMI-1".to_string(), 0, 0, 1920, 1080)));
        assert_eq!(super::parse_monitor(" 1: monctl-1 1280/295x800/184+1920+0  none"), Some(("monctl-1".to_string(), 1920, 0, 1280, 800)));
    }

    #[test]
    fn cvt_reduced_blanking() {
        // `cvt -r 1920 1080`: 138.50 1920 1968 2000 2080 1080 1083 1088 1111
        assert_eq!(super::cvt_rb(1920, 1080), (138.5, 1968, 2000, 2080, 1083, 1088, 1111));
    }

    #[test]
    fn aud_split() {
        let s = [0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x65, 1, 0, 0, 0, 1, 9, 0xf0];
        assert_eq!(super::next_aud(&s, 4), Some(12)); // the 4-byte start code of the second AUD
        assert_eq!(super::next_aud(&s, 0), Some(0));
    }
}
