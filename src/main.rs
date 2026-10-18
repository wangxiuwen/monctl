// monctl — turn individual displays on/off, as if the cable were pulled.
// macOS / Windows / Linux, dependency-free.
//
// usage:
//   monctl list                  show all connected displays
//   monctl off  [ID|main]        disable a display        (default: the non-main one)
//   monctl on   [ID|main|all]    enable display(s)        (default: all that are off)
//   monctl toggle [ID|main]      flip one display
//   monctl solo [ID|main]        keep only one display on (default: main)
//   monctl all                   enable every display
//   monctl forget [ID|all]       drop remembered off-state entries

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

mod common;

use common::{DisplayInfo, OffState};
use std::collections::BTreeSet;

fn fail(msg: &str) -> ! {
    eprintln!("monctl: {msg}");
    std::process::exit(1);
}

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
        Some(t) => match displays.iter().find(|d| d.id == t) {
            Some(d) => d,
            None => fail(&format!("display '{t}' is not connected (run `monctl list`)")),
        },
    };
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
            if ids.is_empty() {
                fail("nothing to enable — every connected display is already on");
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
            if let Some(d) = displays.iter().find(|d| d.id == t) {
                if d.active {
                    fail(&format!("display '{t}' is already on"));
                }
                vec![d.clone()]
            } else {
                // not in the OS list: fine if we disabled it earlier, harmless to try otherwise
                vec![ghost(t, &state)]
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
    for (id, name) in &state {
        if !seen.contains(id) {
            println!("{} {} {} {} {}", pad(id, 15), pad("off*", 9), pad("", 5), pad("-", 24), name);
        }
    }
    if !state.is_empty() {
        println!("(off* = turned off by monctl, bring back with `monctl all`)");
    }
}

fn run_enable(enabled: bool, targets: &[DisplayInfo]) {
    let mut state = common::load_state();
    let mut errors = 0;
    for t in targets {
        match platform::set_enabled(&t.id, enabled) {
            Ok(()) => {
                if enabled {
                    state.remove(&t.id);
                } else {
                    state.insert(t.id.clone(), t.name.clone());
                }
                let verb = if enabled { "enabled" } else { "disabled" };
                println!("{verb}  {}", t.describe());
            }
            Err(e) => {
                eprintln!("monctl: failed to {} {}: {}", if enabled { "enable" } else { "disable" }, t.describe(), e);
                errors += 1;
            }
        }
    }
    common::save_state(&state);
    let now_active = active(&platform::list()).len();
    println!("— {now_active} display{} active now ({} changed)", if now_active == 1 { "" } else { "s" }, targets.len() - errors);
    if errors > 0 {
        std::process::exit(1);
    }
}

fn usage() -> ! {
    print!(
        "monctl — turn individual displays on/off (as if unplugged)\n\
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
state made with off/solo lasts until logout/reboot — `monctl all` brings everything back.\n"
    );
    std::process::exit(0);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("list");
    let token = args.get(1).map(|s| s.as_str());

    let displays = platform::list();

    match cmd {
        "-h" | "--help" | "help" => usage(),
        "list" | "ls" => {
            if displays.is_empty() {
                fail("no display found");
            }
            print_list(&displays);
        }
        "off" => {
            let t = resolve_off_target(token, &displays);
            run_enable(false, &[t]);
        }
        "on" => {
            let targets = resolve_on_targets(token, &displays);
            run_enable(true, &targets);
        }
        "all" => {
            let targets = resolve_on_targets(None, &displays);
            run_enable(true, &targets);
        }
        "toggle" => match token {
            None => {
                let t = resolve_off_target(None, &displays);
                run_enable(false, &[t]);
            }
            Some(t) => match displays.iter().find(|d| d.id == t) {
                Some(d) => run_enable(!d.active, std::slice::from_ref(d)),
                None => {
                    if common::load_state().contains_key(t) {
                        run_enable(true, &[ghost(t, &common::load_state())]);
                    } else {
                        fail(&format!("unknown target '{t}' — use a display id or `main`"));
                    }
                }
            },
        },
        "solo" => {
            let keep = match token {
                None | Some("main") => displays.iter().find(|d| d.main).cloned().unwrap_or_else(|| fail("no main display found")),
                Some(t) => displays.iter().find(|d| d.id == t).cloned().unwrap_or_else(|| fail(&format!("unknown target '{t}'"))),
            };
            if !keep.active {
                run_enable(true, &[keep.clone()]);
            }
            let others: Vec<DisplayInfo> = displays.iter().filter(|d| d.active && d.id != keep.id).cloned().collect();
            if others.is_empty() {
                println!("— {} is the only active display already", keep.name);
            } else {
                run_enable(false, &others);
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
                Some(id) => {
                    if state.remove(id).is_some() {
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
