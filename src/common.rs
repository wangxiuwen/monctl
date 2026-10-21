// Shared types, state file and tiny hand-rolled JSON (the crate is dependency-free).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Print an error to stderr and exit(1). Used by every front end.
pub fn fail(msg: &str) -> ! {
    eprintln!("monctl: {msg}");
    std::process::exit(1);
}

/// CLI usage text; also the help back end for `awake` with bad arguments.
pub fn usage() -> ! {
    // raw string: a `\` line continuation would eat each line's indentation
    print!(
        r#"monctl - the screen assistant: turn displays on/off (as if unplugged), resolutions, mirroring, virtual screens

usage:
  monctl list                  show all connected displays
  monctl off  [ID|main]        disable a display        (default: the non-main one)
  monctl on   [ID|main|all]    enable display(s)        (default: all that are off)
  monctl toggle [ID|main]      flip one display
  monctl solo [ID|main]        keep only one display on (default: main)
  monctl all                   enable every connected display
  monctl forget [ID|all]       drop remembered off-state entries

  monctl mode [ID] [WxH[@Hz]|N]  list / switch resolutions (HiDPI included; --lodpi for 1x)
  monctl main ID               make a display the main one (menu bar, Dock / taskbar)
  monctl mirror ID [SOURCE]    show SOURCE (default: main) on display ID; `mirror off` undoes
  monctl hidpi [ID] | off [ID] macOS: crisp HiDPI rendering on any monitor
  monctl virtual [add [WxH[@Hz]] [--name N] [--no-hidpi] | rm ID|all]   macOS: virtual displays

  monctl awake [DURATION]      prevent sleep (2h, 90m, 2h30m, 3600; default: until off)
  monctl awake off|status      release / inspect the hold (--display also keeps the screen on)
  monctl awake display on|off  keep the screen on (or not) without changing the duration
  monctl sleep                 suspend the machine now

state made with off/solo lasts until logout/reboot - `monctl all` brings everything back.
"#
    );
    std::process::exit(0);
}

/// One connected display. `id` is the platform key used to toggle it
/// (CGDirectDisplayID on macOS, `\\.\DISPLAYn` on Windows, connector name on Linux).
#[derive(Clone, Debug)]
pub struct DisplayInfo {
    pub id: String,
    pub active: bool,
    pub main: bool,
    pub name: String,
    pub detail: String,
    /// id of the display this one mirrors (shows the same picture as)
    pub mirror_of: Option<String>,
}

impl DisplayInfo {
    pub fn describe(&self) -> String {
        format!("{} \"{}\" ({})", self.id, self.name, self.detail)
    }
}

/// One display mode. `width`/`height` is what the desktop looks like
/// (points); `pixel_*` is what is rendered - twice as big for HiDPI modes.
#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub hz: u32,
    pub current: bool,
}

impl Mode {
    pub fn hidpi(&self) -> bool {
        self.pixel_width > self.width
    }

    pub fn describe(&self) -> String {
        let hz = if self.hz > 0 { format!(" @{}Hz", self.hz) } else { String::new() };
        let dpi = if self.hidpi() { format!("  HiDPI ({}x{})", self.pixel_width, self.pixel_height) } else { String::new() };
        format!("{}x{}{hz}{dpi}", self.width, self.height)
    }
}

/// Pick the mode a user means by "WxH[@Hz]": exact size, preferring the
/// requested refresh rate (else the highest), then HiDPI unless `lodpi`.
pub fn pick_mode(modes: &[Mode], w: u32, h: u32, hz: Option<u32>, lodpi: bool) -> Option<&Mode> {
    modes
        .iter()
        .filter(|m| m.width == w && m.height == h)
        .filter(|m| hz.is_none_or(|z| m.hz == z || m.hz == 0))
        .max_by_key(|m| (m.hidpi() != lodpi, m.hz))
}

/// Displays turned off by monctl: id -> friendly name.
/// Needed because a disabled display usually vanishes from the OS enumeration,
/// so the only way to bring it back is to remember its id.
pub type OffState = BTreeMap<String, String>;

pub fn state_path() -> PathBuf {
    config_dir().join("monctl/state.json")
}

pub fn awake_path() -> PathBuf {
    config_dir().join("monctl/awake.json")
}

fn config_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    #[cfg(target_os = "macos")]
    let base = home.join("Library/Application Support");
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Roaming"));
    base
}

pub fn load_state() -> OffState {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|s| json_parse_object(&s))
        .unwrap_or_default()
}

pub fn save_state(state: &OffState) {
    write_atomic(&state_path(), &json_write_object(state));
}

/// Write via a temp file + rename so a concurrent reader (tray, holder
/// process, another CLI run) never sees a truncated, half-written file.
pub fn write_atomic(path: &Path, contents: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if fs::write(&tmp, contents).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

// ---- minimal JSON object-of-strings support ----

pub fn json_write_object(map: &OffState) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{}:{}", json_write_string(k), json_write_string(v)));
    }
    out.push('}');
    out
}

fn json_write_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse a flat JSON object whose values are all strings; anything else fails to None.
pub fn json_parse_object(s: &str) -> Option<OffState> {
    let bytes: Vec<char> = s.chars().collect();
    let mut pos = 0usize;
    let mut map = OffState::new();

    fn ws(b: &[char], pos: &mut usize) {
        while *pos < b.len() && b[*pos].is_whitespace() {
            *pos += 1;
        }
    }
    fn string_lit(b: &[char], pos: &mut usize) -> Option<String> {
        if b.get(*pos) != Some(&'"') {
            return None;
        }
        *pos += 1;
        let mut out = String::new();
        loop {
            let c = *b.get(*pos)?;
            *pos += 1;
            match c {
                '"' => return Some(out),
                '\\' => {
                    let e = *b.get(*pos)?;
                    *pos += 1;
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let hex: String = b.get(*pos..*pos + 4)?.iter().collect();
                            *pos += 4;
                            let cp = u32::from_str_radix(&hex, 16).ok()?;
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return None,
                    }
                }
                c => out.push(c),
            }
        }
    }

    ws(&bytes, &mut pos);
    if bytes.get(pos) != Some(&'{') {
        return None;
    }
    pos += 1;
    loop {
        ws(&bytes, &mut pos);
        match bytes.get(pos) {
            Some('}') => return Some(map),
            Some(',') => pos += 1,
            Some('"') => {
                let key = string_lit(&bytes, &mut pos)?;
                ws(&bytes, &mut pos);
                if bytes.get(pos) != Some(&':') {
                    return None;
                }
                pos += 1;
                ws(&bytes, &mut pos);
                let val = string_lit(&bytes, &mut pos)?;
                map.insert(key, val);
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: u32, h: u32, pw: u32, hz: u32) -> Mode {
        Mode { width: w, height: h, pixel_width: pw, pixel_height: pw * h / w, hz, current: false }
    }

    #[test]
    fn pick_prefers_hidpi_and_high_refresh() {
        let modes = [mode(1920, 1080, 1920, 60), mode(1920, 1080, 3840, 60), mode(1920, 1080, 1920, 120), mode(2560, 1440, 2560, 60)];
        assert_eq!(pick_mode(&modes, 1920, 1080, None, false), Some(&modes[1]));
        assert_eq!(pick_mode(&modes, 1920, 1080, None, true), Some(&modes[2]));
        assert_eq!(pick_mode(&modes, 1920, 1080, Some(60), true), Some(&modes[0]));
        assert_eq!(pick_mode(&modes, 1280, 720, None, false), None);
    }

    #[test]
    fn json_roundtrip() {
        let mut m = OffState::new();
        m.insert("5".into(), "DELL P2314H".into());
        m.insert("dp-1".into(), "quote\" back\\ slash".into());
        let s = json_write_object(&m);
        assert_eq!(json_parse_object(&s), Some(m));
    }

    #[test]
    fn json_rejects_non_flat() {
        assert_eq!(json_parse_object("{\"a\": {\"b\":1}}"), None);
        assert_eq!(json_parse_object("[]"), None);
        assert_eq!(json_parse_object("{\"a\": 12}"), None);
    }
}
