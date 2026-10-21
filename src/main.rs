// monctl - turn individual displays on/off, as if the cable were pulled.
// macOS / Windows / Linux, dependency-free. Front end for the monctl lib.

#[cfg(target_os = "macos")]
use monctl::macos as platform;
#[cfg(target_os = "windows")]
use monctl::windows as platform;
#[cfg(target_os = "linux")]
use monctl::linux as platform;

use monctl::common;
use monctl::common::{fail, usage, DisplayInfo, OffState};
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
            let mut ids: BTreeSet<String> = displays.iter().filter(|d| !d.active).map(|d| d.id.clone()).collect();
            ids.extend(state.keys().cloned()); // ids that vanished from the OS entirely
            ids.retain(|id| !displays.iter().any(|d| d.id == *id && d.active));
            // phantom outputs (virtual adapters with no panel) can never be
            // re-attached; don't report them as errors every time
            #[cfg(windows)]
            ids.retain(|id| match displays.iter().find(|d| d.id == *id) {
                Some(_) => monctl::windows::can_attach(id),
                None => true, // remembered-off ghost: a real display we turned off
            });
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

fn print_list(displays: &[DisplayInfo]) {
    fn pad(s: &str, n: usize) -> String {
        if s.chars().count() >= n {
            s.to_string()
        } else {
            format!("{s}{}", " ".repeat(n - s.chars().count()))
        }
    }
    println!("id              state     main  detail                   name");
    for d in displays {
        let state = if d.active { "active" } else { "off" };
        println!(
            "{} {} {} {} {}",
            pad(&d.id, 15),
            pad(state, 9),
            pad(if d.main { "*" } else { "" }, 5),
            pad(&d.detail, 24),
            d.name
        );
    }
    let state = common::load_state();
    let seen: BTreeSet<&String> = displays.iter().map(|d| &d.id).collect();
    let active_names: BTreeSet<&str> = displays.iter().filter(|d| d.active).map(|d| d.name.as_str()).collect();
    for (id, name) in &state {
        // a remembered id gone from the OS while an active display has the
        // same name = the same panel under a new id, not a third screen
        if !seen.contains(id) && !active_names.contains(name.as_str()) {
            println!("{} {} {} {} {}", pad(id, 15), pad("off*", 9), pad("", 5), pad("-", 24), name);
        }
    }
    if !state.is_empty() {
        println!("(off* = turned off by monctl, bring back with `monctl all`)");
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
