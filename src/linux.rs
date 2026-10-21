// Linux backend — X11 via xrandr; KDE Wayland via kscreen-doctor.
// GNOME Wayland has no stable public interface for disabling outputs yet (see README).

use crate::common::DisplayInfo;
use std::process::Command;

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e} (is it installed?)"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn session() -> &'static str {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => "wayland",
        Ok("x11") => "x11",
        _ => {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                "wayland"
            } else if std::env::var_os("DISPLAY").is_some() {
                "x11"
            } else {
                "unknown"
            }
        }
    }
}

// ---- X11 / xrandr ----

fn xrandr_list() -> Result<Vec<DisplayInfo>, String> {
    let out = run("xrandr", &["--query"])?;
    let mut displays = vec![];
    for line in out.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(status) = parts.next() else { continue };
        if status != "connected" && status != "disconnected" {
            continue;
        }
        let rest: Vec<&str> = parts.collect();
        let main = rest.first() == Some(&"primary");
        // "connected" only means a cable is in: an output switched off with
        // `xrandr --off` is still "connected", just without a WxH+X+Y geometry.
        let geometry = rest.iter().find(|t| t.contains('x') && t.contains('+')).map(|g| g.to_string());
        let active = geometry.is_some();
        // nothing plugged in and nothing configured: an empty connector, not a display
        if status == "disconnected" && !active {
            continue;
        }
        displays.push(DisplayInfo {
            id: name.to_string(),
            active,
            main,
            name: name.to_string(),
            detail: geometry.unwrap_or_else(|| "n/a".into()),
        });
    }
    if displays.is_empty() {
        return Err("xrandr reported no outputs".into());
    }
    Ok(displays)
}

// ---- KDE Wayland / kscreen-doctor ----

/// kscreen-doctor colours its output even when piped ("\x1b[01;32mOutput: ...").
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ params... final byte in @..~
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn kscreen_list() -> Result<Vec<DisplayInfo>, String> {
    let out = run("kscreen-doctor", &["-o"])?;
    let mut displays = vec![];
    for line in strip_ansi(&out).lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.first() != Some(&"Output:") || t.len() < 4 {
            continue;
        }
        let id = t[2].to_string(); // connector name, e.g. DP-1
        let enabled = t.iter().any(|&s| s == "enabled");
        let connected = t.iter().any(|&s| s == "connected");
        if !connected {
            continue; // empty connector
        }
        let priority = t.iter().position(|&s| s == "priority").and_then(|i| t.get(i + 1).copied());
        let main = priority == Some("1");
        displays.push(DisplayInfo {
            id: id.clone(),
            active: enabled && connected,
            main,
            name: id,
            detail: if enabled { "enabled".into() } else { "disabled".into() },
        });
    }
    if displays.is_empty() {
        return Err("kscreen-doctor reported no outputs".into());
    }
    Ok(displays)
}

pub fn list() -> Vec<DisplayInfo> {
    let result = match session() {
        "wayland" => kscreen_list().or_else(|_| xrandr_list()),
        _ => xrandr_list().or_else(|_| kscreen_list()),
    };
    result.unwrap_or_else(|e| {
        eprintln!("monctl: {e}");
        if session() == "wayland" {
            eprintln!("monctl: on GNOME Wayland disabling outputs is not supported yet — use an X11 session or KDE (kscreen-doctor)");
        }
        std::process::exit(1);
    })
}

pub fn set_enabled(id: &str, enable: bool) -> Result<(), String> {
    let state = if enable { "enable" } else { "disable" };
    match session() {
        "wayland" => run("kscreen-doctor", &[&format!("output.{id}.{state}")]).map(|_| ()),
        _ => {
            if enable {
                run("xrandr", &["--output", id, "--auto"]).map(|_| ())
            } else {
                run("xrandr", &["--output", id, "--off"]).map(|_| ())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn strip_ansi_removes_csi() {
        assert_eq!(super::strip_ansi("\x1b[01;32mOutput: \x1b[0;0m1 DP-1 enabled"), "Output: 1 DP-1 enabled");
    }
}
