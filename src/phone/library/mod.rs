// The app library: the phone display's home page. The running apps and
// every installed app on this machine, searchable by name; tapping one
// opens it (its windows land wherever the system puts them - the user then
// places what they want on the phone display).
//
// The page is a plain web page served on localhost; the display window is
// a web view pointed at it (WKWebView on macOS, a GDI list on Windows for
// now - it looks like a settings page, not an iPhone home screen, and is
// honest about that). Interactions go over the same HTTP server, so no
// view-bridge protocols are needed anywhere.

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(windows)]
pub mod win;

#[cfg(target_os = "macos")]
pub use mac::{hide, refit, show};
#[cfg(windows)]
pub use win::{hide, show};

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

pub const PORT: u16 = 8485;

/// One app: what the page and the open/close actions need.
#[derive(Clone, Debug)]
pub struct App {
    pub name: String,
    /// pid when running (running section), 0 otherwise.
    pub pid: u32,
    /// Whether it is in the running section.
    pub running: bool,
}

/// What the platform layer provides: enumerate and launch.
pub trait Catalog {
    fn apps(&self) -> Vec<App>;
    fn open(&self, name: &str) -> Result<(), String>;
    /// Best-effort quit by name (used by the page's long-press action).
    fn quit(&self, name: &str) -> Result<(), String>;
}

impl<T: Catalog + Send + Sync> Catalog for std::sync::Arc<T> {
    fn apps(&self) -> Vec<App> {
        (**self).apps()
    }
    fn open(&self, name: &str) -> Result<(), String> {
        (**self).open(name)
    }
    fn quit(&self, name: &str) -> Result<(), String> {
        (**self).quit(name)
    }
}

/// `monctl phone library` - the phone display's home page, until the
/// session ends. Watch shows it when a phone connects.
pub fn run_cli() {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = mac::show() {
            crate::common::fail(&e);
        }
        println!("app library up on http://127.0.0.1:{}/ - Ctrl-C to close", PORT);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    #[cfg(windows)]
    {
        if let Err(e) = win::show() {
            crate::common::fail(&e);
        }
    }
}

pub fn run_server(catalog: impl Catalog + Send + Sync + 'static) -> std::io::Result<()> {
    let catalog = std::sync::Arc::new(catalog);
    let listener = TcpListener::bind(("127.0.0.1", PORT))?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let Ok(mut s) = stream.try_clone() else { continue };
            let catalog = catalog.clone();
            std::thread::spawn(move || {
                let Ok(clone) = s.try_clone() else { return };
                let mut reader = BufReader::new(clone);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                // drain headers
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h == "\r\n" || h == "\n" || h.is_empty() {
                        break;
                    }
                }
                let words: Vec<&str> = line.split_whitespace().collect();
                let Some(parts) = words.get(..2) else { return };
                let (method, path) = (parts[0].to_string(), parts[1].to_string());
                let path = path.split('?').next().unwrap_or("").to_string();
                let answer = match (method.as_str(), path.as_str()) {
                    ("GET", "/") => ("200", "text/html; charset=utf-8", page(&catalog)),
                    ("GET", "/apps.json") => ("200", "application/json", apps_json(&catalog)),
                    ("POST", p) if p.starts_with("/open/") => {
                        let name = url_decode(&p["/open/".len()..]);
                        match crate::phone::window::open_and_place(&catalog, &name) {
                            Ok(msg) => ("200", "text/plain", msg),
                            Err(e) => ("500", "text/plain", e),
                        }
                    }
                    ("POST", p) if p.starts_with("/quit/") => {
                        let name = url_decode(&p["/quit/".len()..]);
                        match catalog.quit(&name) {
                            Ok(()) => ("200", "text/plain", "ok".into()),
                            Err(e) => ("500", "text/plain", e),
                        }
                    }
                    _ => ("404", "text/plain", "not found".into()),
                };
                let body = answer.2;
                let _ = write!(s, "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", answer.0, answer.1, body.len(), body);
                let _ = s.flush();
            });
        }
    });
    Ok(())
}

fn apps_json(catalog: &impl Catalog) -> String {
    let apps = catalog.apps();
    let rows: Vec<String> = apps
        .iter()
        .map(|a| {
            format!(
                "{{\"name\":{},\"running\":{},\"pid\":{}}}",
                super::json::Json::Str(a.name.clone()).write(),
                a.running,
                a.pid
            )
        })
        .collect();
    format!("{{\"apps\":[{}]}}", rows.join(","))
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn page(catalog: &impl Catalog) -> String {
    PAGE.replace("__APPS_JSON__", &apps_json(catalog))
}

const PAGE: &str = include_str!("page.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_decode_roundtrips_chinese_and_spaces() {
        assert_eq!(url_decode("%E5%A4%87%E5%BF%98%E5%BD%95"), "备忘录");
        assert_eq!(url_decode("app+name"), "app name");
        assert_eq!(url_decode("plain"), "plain");
    }
}
