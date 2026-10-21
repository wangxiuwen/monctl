// Linux backend — X11 via xrandr; KDE Wayland via kscreen-doctor.
// GNOME Wayland has no stable public interface for disabling outputs yet (see README).

use crate::common::{DisplayInfo, Mode};
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
            mirror_of: None,
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
            mirror_of: None,
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

// ---- modes / primary / mirroring ----

fn mode(w: u32, h: u32, hz: u32, current: bool) -> Mode {
    Mode { width: w, height: h, pixel_width: w, pixel_height: h, hz, current }
}

/// Mode lines under an output's header in `xrandr --query`:
/// "   1920x1080     60.00*+  59.94    50.00"
fn parse_xrandr_modes(out: &str, id: &str) -> Vec<Mode> {
    let mut modes = vec![];
    let mut inside = false;
    for line in out.lines() {
        if !line.starts_with(' ') {
            inside = line.split_whitespace().next() == Some(id);
            continue;
        }
        if !inside {
            continue;
        }
        let mut t = line.split_whitespace();
        let Some((w, h)) = t.next().and_then(|s| s.split_once('x')) else { continue };
        let (Ok(w), Ok(h)) = (w.parse(), h.trim_end_matches('i').parse()) else { continue };
        for rate in t {
            let current = rate.contains('*');
            if let Ok(hz) = rate.trim_end_matches(['*', '+']).parse::<f64>() {
                modes.push(mode(w, h, hz.round() as u32, current));
            }
        }
    }
    modes
}

/// kscreen-doctor: "Modes:  1:1920x1080@60*!  2:1280x720@60 ..."
fn parse_kscreen_modes(out: &str, id: &str) -> Vec<Mode> {
    let mut modes = vec![];
    let mut inside = false;
    for line in strip_ansi(out).lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.first() == Some(&"Output:") {
            inside = t.get(2) == Some(&id);
            continue;
        }
        if !inside || t.first() != Some(&"Modes:") {
            continue;
        }
        for m in &t[1..] {
            let spec = m.split_once(':').map_or(*m, |(_, s)| s);
            let current = spec.contains('*');
            let spec = spec.trim_end_matches(['*', '!']);
            let Some((size, hz)) = spec.split_once('@') else { continue };
            let Some((w, h)) = size.split_once('x') else { continue };
            if let (Ok(w), Ok(h), Ok(hz)) = (w.parse(), h.parse(), hz.parse::<f64>()) {
                modes.push(mode(w, h, hz.round() as u32, current));
            }
        }
    }
    modes
}

pub fn modes(id: &str) -> Result<Vec<Mode>, String> {
    let modes = match session() {
        "wayland" => parse_kscreen_modes(&run("kscreen-doctor", &["-o"])?, id),
        _ => parse_xrandr_modes(&run("xrandr", &["--query"])?, id),
    };
    if modes.is_empty() {
        return Err(format!("{id} reports no modes"));
    }
    Ok(modes)
}

pub fn set_mode(id: &str, m: &Mode) -> Result<(), String> {
    let size = format!("{}x{}", m.width, m.height);
    match session() {
        "wayland" => run("kscreen-doctor", &[&format!("output.{id}.mode.{size}@{}", m.hz)]).map(|_| ()),
        _ => run("xrandr", &["--output", id, "--mode", &size, "--rate", &m.hz.to_string()]).map(|_| ()),
    }
}

pub fn set_main(id: &str) -> Result<(), String> {
    match session() {
        "wayland" => run("kscreen-doctor", &[&format!("output.{id}.priority.1")]).map(|_| ()),
        _ => run("xrandr", &["--output", id, "--primary"]).map(|_| ()),
    }
}

pub fn set_mirror(id: &str, master: Option<&str>) -> Result<(), String> {
    if session() == "wayland" {
        return Err("mirroring from the command line needs X11 (xrandr --same-as)".into());
    }
    match master {
        Some(m) => run("xrandr", &["--output", id, "--same-as", m]).map(|_| ()),
        // back to its own spot: right of the primary output
        None => {
            let primary = xrandr_list()?.into_iter().find(|d| d.main && d.id != id);
            match primary {
                Some(p) => run("xrandr", &["--output", id, "--auto", "--right-of", &p.id]).map(|_| ()),
                None => run("xrandr", &["--output", id, "--auto"]).map(|_| ()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn xrandr_modes() {
        let out = "HDMI-1 connected primary 1920x1080+0+0 (normal) 527mm x 296mm\n   1920x1080     60.00*+  50.00\n   1280x720      60.00\nDP-1 disconnected (normal)\n   1024x768      60.00\n";
        let m = super::parse_xrandr_modes(out, "HDMI-1");
        assert_eq!(m.len(), 3);
        assert!(m[0].current && m[0].hz == 60 && m[0].width == 1920);
        assert_eq!(m[2].width, 1280);
    }

    #[test]
    fn kscreen_modes() {
        let out = "Output: 1 DP-1 enabled connected priority 1 DisplayPort\n\tModes:  1:2560x1440@60*!  2:1920x1080@59.94\n";
        let m = super::parse_kscreen_modes(out, "DP-1");
        assert_eq!(m.len(), 2);
        assert!(m[0].current && m[0].width == 2560);
        assert_eq!(m[1].hz, 60);
    }

    #[test]
    fn strip_ansi_removes_csi() {
        assert_eq!(super::strip_ansi("\x1b[01;32mOutput: \x1b[0;0m1 DP-1 enabled"), "Output: 1 DP-1 enabled");
    }
}
