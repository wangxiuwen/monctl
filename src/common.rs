// Shared types, state file and tiny hand-rolled JSON (the crate is dependency-free).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Print an error to stderr and exit(1). Used by every front end.
pub fn fail(msg: &str) -> ! {
    eprintln!("monctl: {msg}");
    std::process::exit(1);
}

/// CLI usage text; also the help back end for `awake` with bad arguments.
pub fn usage() -> ! {
    print!(
        "monctl - turn individual displays on/off (as if unplugged)\n\
\n\
usage:\n\
  monctl list                  show all connected displays\n\
  monctl off  [ID|main]        disable a display        (default: the non-main one)\n\
  monctl on   [ID|main|all]    enable display(s)        (default: all that are off)\n\
  monctl toggle [ID|main]      flip one display\n\
  monctl solo [ID|main]        keep only one display on (default: main)\n\
  monctl all                   enable every connected display\n\
  monctl forget [ID|all]       drop remembered off-state entries\n\
\n\
  monctl awake [DURATION]      prevent sleep (2h, 90m, 2h30m, 3600; default: until off)\n\
  monctl awake off|status      release / inspect the hold (--display also keeps the screen on)\n\
  monctl sleep                 suspend the machine now\n\
\n\
state made with off/solo lasts until logout/reboot - `monctl all` brings everything back.\n"
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
}

impl DisplayInfo {
    pub fn describe(&self) -> String {
        format!("{} \"{}\" ({})", self.id, self.name, self.detail)
    }
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
    let path = state_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, json_write_object(state));
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
