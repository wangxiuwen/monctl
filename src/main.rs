// monctl - turn individual displays on/off, as if the cable were pulled.
// macOS / Windows / Linux, dependency-free. Front end for the monctl lib.

#[cfg(target_os = "macos")]
use monctl::macos as platform;
#[cfg(target_os = "windows")]
use monctl::windows as platform;
#[cfg(target_os = "linux")]
use monctl::linux as platform;

use monctl::common;
use monctl::common::{fail, usage, DisplayInfo, Mode, OffState};
use monctl::awake;

use std::collections::BTreeSet;

fn active(displays: &[DisplayInfo]) -> Vec<&DisplayInfo> {
    displays.iter().filter(|d| d.active).collect()
}

fn ghost(id: &str, state: &OffState) -> DisplayInfo {
    DisplayInfo {
        id: id.to_string(),
        active: false,
        main: false,
        name: state.get(id).cloned().unwrap_or_else(|| "Display".into()),
        detail: "n/a".into(),
        mirror_of: None,
    }
}

/// Whether a user-typed token names this platform id: the exact id
/// (`\\.\DISPLAY2`), or the short forms people type from `monctl list`
/// output (`DISPLAY2`, `display2`, `2`). Whole-name match only - a suffix
/// match would let `1` hit `\\.\DISPLAY11`.
fn id_matches(id: &str, token: &str) -> bool {
    if id == token {
        return true;
    }
    let short = id.strip_prefix("\\\\.\\").unwrap_or(id);
    short.eq_ignore_ascii_case(token)
        || short
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("DISPLAY") && short[7..] == *token)
}

fn find_display<'a>(displays: &'a [DisplayInfo], token: &str) -> Option<&'a DisplayInfo> {
    displays.iter().find(|d| id_matches(&d.id, token))
}

/// Map a token for a display that vanished from the OS to the id we stored
/// when turning it off, so `monctl on 2` works for `\\.\DISPLAY2` too.
fn remembered_id(token: &str, state: &OffState) -> String {
    state.keys().find(|id| id_matches(id, token)).cloned().unwrap_or_else(|| token.to_string())
}

fn resolve_off_target(token: Option<&str>, displays: &[DisplayInfo]) -> DisplayInfo {
    let act = active(displays);
    if act.is_empty() {
        fail("no active display to turn off");
    }
    let target = match token {
        None | Some("secondary" | "second" | "other") => {
            act.iter().find(|d| !d.main).or_else(|| act.first()).unwrap()
        }
        Some("main") => act.iter().find(|d| d.main).unwrap_or_else(|| {
            fail("no active main display found");
        }),
        Some(t) => match find_display(displays, t) {
            Some(d) => d,
            None => fail(&format!("display '{t}' is not connected (run `monctl list`)")),
        },
    };
    if !target.active {
        fail(&format!("display {} is already off", target.describe()));
    }
    if act.len() <= 1 && act[0].id == target.id {
        fail("refusing to turn off the last active screen");
    }
    target.clone()
}

fn resolve_on_targets(token: Option<&str>, displays: &[DisplayInfo]) -> Vec<DisplayInfo> {
    let state = common::load_state();
    match token {
        None | Some("all") => {
            // a mirror shows another display's picture: it is on, not "off"
            let mut ids: BTreeSet<String> = displays.iter().filter(|d| !d.active && d.mirror_of.is_none()).map(|d| d.id.clone()).collect();
            ids.extend(state.keys().cloned()); // ids that vanished from the OS entirely
            ids.retain(|id| !displays.iter().any(|d| d.id == *id && d.active));
            // phantom outputs (virtual adapters with no panel) can never be
            // re-attached; don't report them as errors every time
            #[cfg(windows)]
            {
                // the virtual display driver's parked monitor belongs to `cast`
                let vdd = monctl::windows::adapters_named(monctl::winvdd::ADAPTER_NAME, false);
                ids.retain(|id| match displays.iter().find(|d| d.id == *id) {
                    Some(_) => monctl::windows::can_attach(id) && !vdd.contains(id),
                    None => true, // remembered-off ghost: a real display we turned off
                });
            }
            if ids.is_empty() {
                fail("nothing to enable - every connected display is already on");
            }
            ids.iter()
                .map(|id| {
                    displays
                        .iter()
                        .find(|d| d.id == *id)
                        .cloned()
                        .unwrap_or_else(|| ghost(id, &state))
                })
                .collect()
        }
        Some("main") => match displays.iter().find(|d| d.main && !d.active) {
            Some(d) => vec![d.clone()],
            None => fail("the main display is already on (or not connected)"),
        },
        Some(t) => {
            if let Some(d) = find_display(displays, t) {
                if d.active {
                    fail(&format!("display '{t}' is already on"));
                }
                vec![d.clone()]
            } else {
                // not in the OS list: fine if we disabled it earlier, harmless to try otherwise
                vec![ghost(&remembered_id(t, &state), &state)]
            }
        }
    }
}

/// Any currently connected display by id / short id / `main`.
fn resolve_any(token: Option<&str>, displays: &[DisplayInfo]) -> DisplayInfo {
    match token {
        None | Some("main") => displays.iter().find(|d| d.main).cloned().unwrap_or_else(|| fail("no main display found")),
        Some(t) => find_display(displays, t).cloned().unwrap_or_else(|| fail(&format!("display '{t}' is not connected (run `monctl list`)"))),
    }
}

/// Modes worth showing: deduplicated, biggest first, HiDPI before 1x.
fn distinct_modes(mut modes: Vec<Mode>) -> Vec<Mode> {
    modes.sort_by_key(|m| std::cmp::Reverse((m.width, m.height, m.hidpi(), m.hz, m.current)));
    modes.dedup_by(|b, a| a.width == b.width && a.height == b.height && a.hidpi() == b.hidpi() && a.hz == b.hz);
    modes
}

fn cmd_mode(args: &[String], displays: &[DisplayInfo]) {
    let lodpi = args.iter().any(|a| a == "--lodpi" || a == "--no-hidpi");
    let rest: Vec<&str> = args.iter().map(|s| s.as_str()).filter(|a| !a.starts_with("--")).collect();
    // `mode 2560x1440` (main display) or `mode ID 2560x1440` / `mode ID 3`
    let (target, spec) = match rest.as_slice() {
        [] => (None, None),
        [one] if one.contains('x') || one.contains('@') => (None, Some(*one)),
        [one] => (Some(*one), None),
        [t, s, ..] => (Some(*t), Some(*s)),
    };
    let d = resolve_any(target, displays);
    let modes = distinct_modes(platform::modes(&d.id).unwrap_or_else(|e| fail(&e)));
    let Some(spec) = spec else {
        println!("modes of {}:", d.describe());
        for (i, m) in modes.iter().enumerate() {
            println!("{} {:>3}  {}", if m.current { "*" } else { " " }, i + 1, m.describe());
        }
        println!("(switch: monctl mode {} WxH[@Hz]  or  monctl mode {} N)", d.id, d.id);
        return;
    };
    let mode = if let Ok(n) = spec.parse::<usize>() {
        modes.get(n.wrapping_sub(1)).unwrap_or_else(|| fail(&format!("no mode #{n} (see `monctl mode {}`)", d.id)))
    } else {
        let (size, hz) = match spec.split_once('@') {
            Some((a, b)) => (a, Some(b.trim_end_matches(['H', 'h', 'z']).parse().unwrap_or_else(|_| fail(&format!("bad refresh rate in '{spec}'"))))),
            None => (spec, None),
        };
        let (w, h) = size
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or_else(|| fail(&format!("bad mode '{spec}' - use WxH, WxH@Hz or a number from `monctl mode {}`", d.id)));
        common::pick_mode(&modes, w, h, hz, lodpi).unwrap_or_else(|| fail(&format!("{} has no {spec} mode (see `monctl mode {}`)", d.name, d.id)))
    };
    if mode.current {
        println!("- {} is already at {}", d.name, mode.describe());
        return;
    }
    platform::set_mode(&d.id, mode).unwrap_or_else(|e| fail(&e));
    println!("{}  -> {}", d.describe(), mode.describe());
}

/// HiDPI on any screen: render the desktop at 2x on a virtual display of the
/// same size and let the real screen mirror it, downsampled - crisp text on
/// monitors macOS refuses to treat as Retina. `hidpi off` undoes it; so does
/// a crashed holder or a reboot, since the virtual display dies with it.
#[cfg(target_os = "macos")]
fn cmd_hidpi(token: Option<&str>, extra: Option<&str>, displays: &[DisplayInfo]) {
    use monctl::virt;
    let tag = |d: &DisplayInfo| format!("HiDPI {}", d.id);
    if token == Some("off") || extra == Some("off") {
        let which = if token == Some("off") { extra } else { token };
        let vs: Vec<virt::Virtual> = virt::live()
            .into_iter()
            .filter(|v| v.name.starts_with("HiDPI ") && which.is_none_or(|t| t == "all" || v.name == format!("HiDPI {t}")))
            .collect();
        if vs.is_empty() {
            println!("- no HiDPI emulation running");
        }
        for v in vs {
            virt::remove(&v);
            println!("hidpi off for display {}", v.name.trim_start_matches("HiDPI "));
        }
        return;
    }
    let d = resolve_any(token, displays);
    if d.mirror_of.is_some() || virt::live().iter().any(|v| v.name == tag(&d)) {
        fail(&format!("{} is already mirroring something (monctl hidpi off {})", d.describe(), d.id));
    }
    let cur = platform::modes(&d.id)
        .unwrap_or_else(|e| fail(&e))
        .into_iter()
        .find(|m| m.current)
        .unwrap_or_else(|| fail("cannot read the current mode"));
    if cur.hidpi() {
        fail(&format!("{} already renders HiDPI ({})", d.name, cur.describe()));
    }
    let spec = virt::Spec { width: cur.width, height: cur.height, hz: cur.hz.max(30), hidpi: true };
    let v = virt::add(spec, Some(tag(&d))).unwrap_or_else(|e| fail(&format!("cannot create the HiDPI canvas: {e}")));
    let vid = v.display_id.to_string();
    // This process's CoreGraphics cache never learns about a display created
    // after it connected (a run loop pump doesn't help), so configure the
    // new canvas from fresh monctl processes, retrying until it shows up.
    let step = |args: &[&str]| -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut last = String::new();
        for _ in 0..30 {
            let out = std::process::Command::new(&exe).args(args).output().map_err(|e| e.to_string())?;
            if out.status.success() {
                return Ok(());
            }
            last = String::from_utf8_lossy(&out.stderr).trim().to_string();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(last)
    };
    let size = format!("{}x{}", cur.width, cur.height);
    let mut steps: Vec<Vec<&str>> = vec![vec!["mode", &vid, &size], vec!["mirror", &d.id, &vid]];
    // mirroring re-lays-out the desktop and may hand "main" to another
    // screen, so reclaim it for the mirror set afterwards
    if d.main {
        steps.push(vec!["main", &vid]);
    }
    for s in &steps {
        if let Err(e) = step(s) {
            virt::remove(&v);
            fail(&format!("hidpi setup failed at `monctl {}`: {e}", s.join(" ")));
        }
    }
    println!("hidpi on  {} - now rendered at {}x{} (undo: monctl hidpi off {})", d.describe(), cur.width * 2, cur.height * 2, d.id);
}

/// What `list` shows: displays in use, plus ones monctl turned off. Idle GPU
/// heads and the outputs of virtual adapters other apps install (Windows
/// lists every one of them as a display) are noise unless asked for (--all).
fn listed(displays: &[DisplayInfo], all: bool) -> Vec<&DisplayInfo> {
    let state = common::load_state();
    displays.iter().filter(|d| all || d.active || d.mirror_of.is_some() || state.contains_key(&d.id)).collect()
}

fn print_list(displays: &[DisplayInfo]) {
    fn pad(s: &str, n: usize) -> String {
        if s.chars().count() >= n {
            s.to_string()
        } else {
            format!("{s}{}", " ".repeat(n - s.chars().count()))
        }
    }
    let all = std::env::args().any(|a| a == "--all" || a == "-a");
    let shown = listed(displays, all);
    println!("id              state     main  detail                         name");
    for d in &shown {
        let state = if d.mirror_of.is_some() {
            "mirror"
        } else if d.active {
            "active"
        } else {
            "off"
        };
        println!(
            "{} {} {} {} {}",
            pad(&d.id, 15),
            pad(state, 9),
            pad(if d.main { "*" } else { "" }, 5),
            pad(&d.detail, 30),
            d.name
        );
    }
    let state = common::load_state();
    let seen: BTreeSet<&String> = displays.iter().map(|d| &d.id).collect();
    let active_names: BTreeSet<&str> = displays.iter().filter(|d| d.active).map(|d| d.name.as_str()).collect();
    let mut ghosts = 0;
    for (id, name) in &state {
        // a remembered id gone from the OS while an active display has the
        // same name = the same panel under a new id, not a third screen
        if !seen.contains(id) && !active_names.contains(name.as_str()) {
            println!("{} {} {} {} {}", pad(id, 15), pad("off*", 9), pad("", 5), pad("-", 30), name);
            ghosts += 1;
        }
    }
    if ghosts > 0 {
        println!("(off* = turned off by monctl, bring back with `monctl all`)");
    }
    let hidden = displays.len() - shown.len();
    if hidden > 0 {
        println!("({hidden} idle output{} hidden - `monctl list --all` shows them)", if hidden == 1 { "" } else { "s" });
    }
}

fn run_enable(enabled: bool, targets: &[DisplayInfo], explicit: bool) {
    let mut state = common::load_state();
    let mut errors = 0;
    let mut changed = 0;
    for t in targets {
        // Windows drivers may accept detaching the primary display but refuse
        // the re-attach afterwards (DISP_CHANGE_FAILED), leaving it dark until
        // reboot — verified on NVIDIA. Never close the primary there.
        #[cfg(windows)]
        if !enabled && t.main {
            eprintln!(
                "monctl: refusing to turn off the primary display {} - Windows drivers may not bring it back until reboot (turn off a secondary display instead)",
                t.describe()
            );
            errors += 1;
            continue;
        }
        match platform::set_enabled(&t.id, enabled) {
            Ok(()) => {
                if enabled {
                    state.remove(&t.id);
                } else {
                    state.insert(t.id.clone(), t.name.clone());
                }
                changed += 1;
                let verb = if enabled { "enabled" } else { "disabled" };
                println!("{verb}  {}", t.describe());
            }
            Err(e) => {
                if explicit {
                    eprintln!("monctl: failed to {} {}: {}", if enabled { "enable" } else { "disable" }, t.describe(), e);
                    errors += 1;
                } else {
                    // bulk commands sweep up phantom outputs (GPU heads /
                    // virtual adapters with no panel) that can never attach;
                    // they are noise, not errors. A named target still fails
                    // loudly above.
                    println!("- skipped {} (cannot be attached: {e})", t.describe());
                }
            }
        }
    }
    common::save_state(&state);
    let now_active = active(&platform::list()).len();
    println!("- {now_active} display{} active now ({} changed)", if now_active == 1 { "" } else { "s" }, changed);
    if errors > 0 {
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("list");
    let token = args.get(1).map(|s| s.as_str());

    // awake/sleep don't touch displays - and on Windows a service context
    // (session 0) can't enumerate them at all, so dispatch first.
    match cmd {
        "-h" | "--help" | "help" => usage(),
        "awake" => awake::run(&args[1..]),
        "sleep" => awake::sleep_now(),
        #[cfg(target_os = "macos")]
        "virtual" | "virt" => monctl::virt::run(&args[1..]),
        "cast" | "tablet" => monctl::cast::run(&args[1..]),
        #[cfg(windows)]
        "driver" => monctl::winvdd::run_cli(&args[1..]),
        #[cfg(target_os = "macos")]
        "sidecar" | "ipad" => monctl::sidecar::run(&args[1..]),
        #[cfg(not(target_os = "macos"))]
        "sidecar" | "ipad" => fail("Sidecar (iPad as a display) is Apple's, macOS only - on Windows use `monctl cast`"),
        #[cfg(not(target_os = "macos"))]
        "virtual" | "virt" => fail("virtual displays are macOS-only for now (Windows needs an indirect display driver)"),
        _ => {}
    }

    let displays = platform::list();

    match cmd {
        "list" | "ls" => {
            if displays.is_empty() {
                fail("no display found");
            }
            print_list(&displays);
        }
        "off" => {
            let t = resolve_off_target(token, &displays);
            run_enable(false, &[t], true);
        }
        "on" => {
            let targets = resolve_on_targets(token, &displays);
            // `on all` is the same bulk sweep as `all`: phantom outputs are skipped, not errors
            run_enable(true, &targets, token.is_some_and(|t| t != "all"));
        }
        "all" => {
            let targets = resolve_on_targets(None, &displays);
            run_enable(true, &targets, false);
        }
        "toggle" => match token {
            None => {
                let t = resolve_off_target(None, &displays);
                run_enable(false, &[t], true);
            }
            Some(t) => match find_display(&displays, t) {
                // turning off goes through the same last-screen guard as `off`
                Some(d) if d.active => {
                    let t = resolve_off_target(Some(t), &displays);
                    run_enable(false, &[t], true);
                }
                Some(d) => run_enable(true, std::slice::from_ref(d), true),
                None => {
                    let state = common::load_state();
                    let id = remembered_id(t, &state);
                    if state.contains_key(&id) {
                        run_enable(true, &[ghost(&id, &state)], true);
                    } else {
                        fail(&format!("unknown target '{t}' - use a display id or `main`"));
                    }
                }
            },
        },
        "solo" => {
            let keep = match token {
                None | Some("main") => displays.iter().find(|d| d.main).cloned().unwrap_or_else(|| fail("no main display found")),
                Some(t) => find_display(&displays, t).cloned().unwrap_or_else(|| fail(&format!("unknown target '{t}'"))),
            };
            if !keep.active {
                run_enable(true, std::slice::from_ref(&keep), true);
            }
            let others: Vec<DisplayInfo> = displays.iter().filter(|d| d.active && d.id != keep.id).cloned().collect();
            if others.is_empty() {
                println!("- {} is the only active display already", keep.name);
            } else {
                run_enable(false, &others, true);
            }
        }
        "mode" | "res" | "resolution" => cmd_mode(&args[1..], &displays),
        #[cfg(target_os = "macos")]
        "hidpi" => cmd_hidpi(token, args.get(2).map(|s| s.as_str()), &displays),
        #[cfg(not(target_os = "macos"))]
        "hidpi" => fail("HiDPI emulation is macOS-only"),
        "main" | "primary" => {
            let Some(t) = token else { fail("usage: monctl main ID") };
            let d = resolve_any(Some(t), &displays);
            if !d.active {
                fail(&format!("display {} is off - turn it on first", d.describe()));
            }
            if d.main {
                println!("- {} is already the main display", d.name);
                return;
            }
            platform::set_main(&d.id).unwrap_or_else(|e| fail(&e));
            println!("main    {}", d.describe());
        }
        "mirror" => match (token, args.get(2).map(|s| s.as_str())) {
            (None, _) => fail("usage: monctl mirror ID [SOURCE|main]   |   monctl mirror off [ID|all]"),
            (Some("off" | "stop"), which) => {
                let targets: Vec<&DisplayInfo> = match which {
                    None | Some("all") => displays.iter().filter(|d| d.mirror_of.is_some()).collect(),
                    Some(t) => vec![find_display(&displays, t).unwrap_or_else(|| fail(&format!("display '{t}' is not connected")))],
                };
                if targets.is_empty() {
                    println!("- nothing is mirroring");
                }
                for d in targets {
                    platform::set_mirror(&d.id, None).unwrap_or_else(|e| fail(&e));
                    println!("unmirrored {}", d.describe());
                }
            }
            (Some(t), source) => {
                let d = resolve_any(Some(t), &displays);
                let src = resolve_any(source, &displays);
                platform::set_mirror(&d.id, Some(&src.id)).unwrap_or_else(|e| fail(&e));
                println!("mirror  {} now shows {}", d.describe(), src.describe());
            }
        },
        "unmirror" => {
            for d in displays.iter().filter(|d| d.mirror_of.is_some() && token.is_none_or(|t| id_matches(&d.id, t))) {
                platform::set_mirror(&d.id, None).unwrap_or_else(|e| fail(&e));
                println!("unmirrored {}", d.describe());
            }
        }
        "forget" => {
            let mut state = common::load_state();
            match token {
                None | Some("all") => {
                    let n = state.len();
                    state.clear();
                    println!("forgot {n} entr{}", if n == 1 { "y" } else { "ies" });
                }
                Some(t) => {
                    let id = remembered_id(t, &state);
                    if state.remove(&id).is_some() {
                        println!("forgot {id}");
                    } else {
                        fail(&format!("'{id}' is not in the off-state"));
                    }
                }
            }
            common::save_state(&state);
        }
        other => fail(&format!("unknown command '{other}' (try `monctl help`)")),
    }
}

#[cfg(test)]
mod tests {
    use super::id_matches;

    #[test]
    fn id_matching_is_whole_name() {
        assert!(id_matches("\\\\.\\DISPLAY2", "\\\\.\\DISPLAY2"));
        assert!(id_matches("\\\\.\\DISPLAY2", "DISPLAY2"));
        assert!(id_matches("\\\\.\\DISPLAY2", "display2"));
        assert!(id_matches("\\\\.\\DISPLAY2", "2"));
        assert!(!id_matches("\\\\.\\DISPLAY11", "1"));
        assert!(!id_matches("\\\\.\\DISPLAY12", "2"));
        assert!(id_matches("5", "5"));
        assert!(!id_matches("15", "5"));
        assert!(id_matches("DP-1", "dp-1"));
    }
}
