// monctl cast — use any tablet (iPad, Android, Windows) as an extra screen.
//
// Zero install on the tablet: it opens a web page. Per connected tablet the
// host creates a screen shaped like it, captures it, hardware-encodes H.264
// and pushes the access units over a WebSocket. The page decodes with
// WebCodecs (secure contexts: localhost via USB/adb) or Media Source
// Extensions (plain http on the LAN), and sends touches back, which the host
// replays as mouse/keyboard input.
//
// This file is the platform-neutral part (HTTP, WebSocket, the session
// protocol, connection help); mac.rs and win.rs create, capture and drive
// the screens.
//
// Wired: Android over USB is set up automatically with `adb reverse`.

use crate::common::fail;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
#[cfg(unix)]
use std::ffi::{c_char, c_void};

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as platform;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

const PAGE: &str = include_str!("page.html");
const HOST_PAGE: &str = include_str!("host.html");

/// Connected tablets (sessions), for /status and the trays.
pub(crate) static TABLETS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The address tablets should open (first LAN address), for /status and /qr.svg.
static URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub const DEFAULT_PORT: u16 = 8484;

/// What an encoder hands the WebSocket writer.
pub(crate) enum Packet {
    Config { codec: String, avcc: Vec<u8>, width: u32, height: u32 },
    Frame { key: bool, ts_us: u64, data: Vec<u8> },
}

/// The tablet's first message.
#[derive(Clone)]
pub(crate) struct Hello {
    /// the line as sent, to hand on to the worker serving the tablet (macOS)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub raw: String,
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
    /// stable per tablet (localStorage): a tablet that comes back gets its
    /// screen back; macOS also keys the virtual display's identity on it
    pub client: String,
    pub label: String,
}

/// One tablet's screen: the display, its capture + encoder, input into it.
/// Dropping it ends all of that (and removes a screen made for the tablet).
pub(crate) trait Screen {
    fn display(&self) -> String;
    fn request_keyframe(&self);
    fn input(&mut self, msg: &str);
}

pub struct Options {
    pub port: u16,
    /// stream an existing display instead of creating one per tablet
    pub display: Option<String>,
    pub bitrate_mbps: u32,
    pub hidpi: bool,
}

/// AVCDecoderConfigurationRecord: what both WebCodecs (`description`) and
/// an MP4 `avcC` box want. Our access units use 4-byte NAL length prefixes.
pub(crate) fn avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(1);
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    v
}


// ---------------------------------------------------------- websocket ----

pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
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

pub(crate) fn ws_send(out: &mut impl Write, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
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
pub(crate) fn ws_read(r: &mut impl Read) -> Option<(u8, Vec<u8>)> {
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


/// Annex-B (start codes) → AVCC (4-byte lengths), pulling SPS/PPS out.
/// AVCC data (4-byte NAL lengths), the SPS/PPS it carried, keyframe or not.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) type AccessUnit = (Vec<u8>, Option<(Vec<u8>, Vec<u8>)>, bool);

#[cfg_attr(target_os = "macos", allow(dead_code))] // VideoToolbox hands out AVCC already
pub(crate) fn annexb_to_avcc(data: &[u8]) -> AccessUnit {
    let mut nals = vec![];
    let mut i = 0;
    let mut start = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && (data[i + 2] == 1 || (data[i + 2] == 0 && i + 3 < data.len() && data[i + 3] == 1)) {
            let sc = if data[i + 2] == 1 { 3 } else { 4 };
            if let Some(s) = start {
                nals.push(&data[s..i]);
            }
            start = Some(i + sc);
            i += sc;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        nals.push(&data[s..]);
    }
    let (mut sps, mut pps, mut key) = (None, None, false);
    let mut out = Vec::with_capacity(data.len() + 16);
    for n in nals {
        let n = {
            // trailing zero bytes belong to the next start code
            let mut end = n.len();
            while end > 1 && n[end - 1] == 0 {
                end -= 1;
            }
            &n[..end]
        };
        if n.is_empty() {
            continue;
        }
        match n[0] & 0x1f {
            7 => sps = Some(n.to_vec()),
            8 => pps = Some(n.to_vec()),
            9 => {} // access unit delimiter: noise for the page
            t => {
                key |= t == 5;
                out.extend_from_slice(&(n.len() as u32).to_be_bytes());
                out.extend_from_slice(n);
            }
        }
    }
    (out, sps.zip(pps), key)
}

// -------------------------------------------------------------- server ----

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
        // first message: "h <css width> <css height> <devicePixelRatio> <client id> <label...>"
        // (the page only talks after it has seen our 101). Read here, so a
        // tablet that comes back can be routed to the screen it left.
        let hello = match ws_read(&mut reader) {
            Some((1, p)) => parse_hello(&String::from_utf8_lossy(&p)),
            _ => None,
        };
        drop(reader);
        if let Some(hello) = hello {
            platform::dispatch(sock, hello, opts);
        }
    } else if path == "/status" {
        let body = format!(
            "{{\"tablets\":{},\"url\":\"{}\"}}",
            TABLETS.load(std::sync::atomic::Ordering::Relaxed),
            URL.get().map(String::as_str).unwrap_or("")
        );
        reply(&mut sock, "application/json", &body);
    } else if path == "/qr.svg" {
        reply(&mut sock, "image/svg+xml", &crate::qr::svg(URL.get().map(String::as_str).unwrap_or("")));
    } else if path == "/host" {
        reply(&mut sock, "text/html; charset=utf-8", HOST_PAGE);
    } else if path == "/quit" {
        // stop casting (the trays' "stop"): only from this machine
        if sock.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
            reply(&mut sock, "text/plain", "bye");
            platform::shutdown();
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

fn reply(sock: &mut TcpStream, kind: &str, body: &str) {
    let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len());
}

pub(crate) fn parse_hello(raw: &str) -> Option<Hello> {
    let f: Vec<&str> = raw.splitn(6, ' ').collect();
    if f.first() != Some(&"h") || f.len() < 5 {
        return None;
    }
    let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    Some(Hello {
        raw: raw.to_string(),
        width: num(1).round() as u32,
        height: num(2).round() as u32,
        dpr: num(3),
        client: f[4].to_string(),
        label: f.get(5).copied().unwrap_or("tablet").to_string(),
    })
}

/// End a connection because the same tablet connected again (the newer one
/// gets the screen). The page then stops reconnecting this one.
pub(crate) fn replaced(sock: &TcpStream) {
    let mut s = sock;
    let _ = ws_send(&mut s, 1, b"bye replaced");
    let _ = sock.shutdown(std::net::Shutdown::Both);
}

/// Tell the page why it gets no screen.
pub(crate) fn refuse(sock: &TcpStream, hello: &Hello, e: &str) {
    eprintln!("monctl cast: {}: {e}", hello.label);
    let mut s = sock;
    let _ = ws_send(&mut s, 1, format!("err {e}").as_bytes());
}

/// Serve one connected tablet until it leaves: frames out, input in. The
/// screen is dropped (capture stops) when the tablet goes; whether the
/// display itself stays for a while is up to the platform.
pub(crate) fn run_session(sock: TcpStream, hello: &Hello, mut screen: Box<dyn Screen>, rx: Receiver<Packet>) {
    let _ = sock.set_nodelay(true);
    let peer = sock.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    println!("+ {} ({peer}, {}x{} @{}x) -> display {}", hello.label, hello.width, hello.height, hello.dpr, screen.display());
    let (mut reader, writer) = match (sock.try_clone(), sock.try_clone()) {
        (Ok(r), Ok(mut w)) => (BufReader::new(r), std::thread::spawn(move || writer_loop(&mut w, rx))),
        _ => return,
    };
    while let Some((op, payload)) = ws_read(&mut reader) {
        match op {
            1 => {
                let msg = String::from_utf8_lossy(&payload);
                if msg == "key" {
                    screen.request_keyframe();
                } else {
                    screen.input(&msg);
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
    drop(screen); // stops capture; the writer sees the channel close
    let _ = writer.join();
    let _ = sock.shutdown(std::net::Shutdown::Both);
    println!("- {} disconnected", hello.label);
}

fn writer_loop(out: &mut TcpStream, rx: Receiver<Packet>) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let r = match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Packet::Config { codec, avcc, width, height }) => ws_send(out, 1, format!("cfg {codec} {width} {height} {}", base64(&avcc)).as_bytes()),
            Ok(Packet::Frame { key, ts_us, data }) => {
                let mut buf = Vec::with_capacity(data.len() + 9);
                buf.push(key as u8);
                buf.extend_from_slice(&ts_us.to_be_bytes());
                buf.extend_from_slice(&data);
                ws_send(out, 2, &buf)
            }
            // a still screen sends nothing: the heartbeat tells the page the
            // link is alive (and us, eventually, that it isn't)
            Err(RecvTimeoutError::Timeout) => ws_send(out, 1, b"hb"),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if r.is_err() {
            break;
        }
    }
}


// ---------------------------------------------------- connection help ----

/// LAN IPv4 addresses worth printing (no loopback / link-local).
#[cfg(unix)]
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
    // BSD/macOS: {u8 len, u8 family}; Linux: {u16 family}
    #[cfg(not(target_os = "linux"))]
    #[repr(C)]
    struct SockAddrIn {
        len: u8,
        family: u8,
        port: u16,
        addr: [u8; 4],
    }
    #[cfg(target_os = "linux")]
    #[repr(C)]
    struct SockAddrIn {
        family: u16,
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
            if !a.is_null() && (*a).family as u32 == 2 {
                let ip = (*a).addr;
                let name = std::ffi::CStr::from_ptr((*p).name).to_string_lossy();
                // skip loopback, link-local, VPN tunnels and container bridges nobody can reach
                let internal = ["utun", "docker", "br-", "veth", "virbr"].iter().any(|p| name.starts_with(p));
                if ip[0] != 127 && !(ip[0] == 169 && ip[1] == 254) && !internal {
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

/// The address this machine uses on its LAN: route a UDP socket towards a
/// public address (nothing is sent) and read the chosen source address.
#[cfg(windows)]
fn lan_addresses() -> Vec<String> {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("192.0.2.1:9").map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| vec![a.ip().to_string()])
        .unwrap_or_default()
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
    platform::prepare(&opts);
    let addrs = lan_addresses();
    if let Some(a) = addrs.first() {
        let _ = URL.set(format!("http://{a}:{}/", opts.port));
    }
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
            "--display" | "--mirror" => opts.display = Some(platform::resolve_display(&val()).unwrap_or_else(|e| fail(&e))),
            "--bitrate" => opts.bitrate_mbps = val().trim_end_matches('M').parse().unwrap_or_else(|_| fail("bad --bitrate (Mbit/s)")),
            "--no-hidpi" | "--lodpi" => opts.hidpi = false,
            _ => fail("usage: monctl cast [--port N] [--display ID|main] [--bitrate MBPS] [--no-hidpi]"),
        }
    }
    if worker {
        platform::worker(&opts);
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
    fn annexb_split() {
        let data = [0, 0, 0, 1, 0x67, 0x4d, 0, 0x28, 0, 0, 1, 0x68, 0xee, 0, 0, 0, 1, 0x65, 1, 2, 3];
        let (avcc_data, params, key) = annexb_to_avcc(&data);
        assert!(key);
        assert_eq!(params, Some((vec![0x67, 0x4d, 0, 0x28], vec![0x68, 0xee])));
        assert_eq!(avcc_data, vec![0, 0, 0, 4, 0x65, 1, 2, 3]);
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

