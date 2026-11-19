// The automation loop: watch the detectors, and when a phone starts
// controlling this machine, add the phone display (and put the app library
// up); when the session ends, put the windows back and remove it.
//
// The timing discipline lives in detect::Fusion; this loop only feeds it.

use super::display::{self};
use super::detect::{registry, Fusion, Phase, Signal};
use super::fit::Fit;
use super::window;
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn run_cli(args: &[String]) {
    let mut choice = super::Choice::from_flags(
        args.iter().position(|a| a == "--model").and_then(|i| args.get(i + 1).map(String::as_str)),
        args.iter().position(|a| a == "--size").and_then(|i| args.get(i + 1)).and_then(|v| {
            v.split_once('x').and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
        }),
        args.iter().position(|a| a == "--points").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()),
        args.iter().position(|a| a == "--ratio").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()),
    );
    if choice.is_none() {
        choice = super::load_choice();
    }
    let choice = choice.unwrap_or_else(|| {
        println!("no phone chosen yet - using 540x950 (set one: monctl phone on --model \"iPhone 17 Pro Max\")");
        super::Choice::Ratio(1.759)
    });
    super::save_choice(&choice);

    let mut detectors = registry();
    println!("watching: {} ({})", detectors.iter().map(|d| d.name()).collect::<Vec<_>>().join(", "), choice_desc(&choice));
    unsafe {
        signal(2, on_sigint as usize); // SIGINT
    }

    let mut fusion = Fusion::new();
    let mut last_phase = Phase::Idle;
    loop {
        let now = now_secs();
        let mut signals: Vec<Signal> = Vec::new();
        let mut inset = super::fit::DEFAULT_INSET;
        for d in detectors.iter_mut() {
            for s in d.poll() {
                inset = d.inset();
                signals.push(s);
            }
        }
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        match fusion.tick(now, &signals) {
            super::detect::Action::Add => {
                let fit = Fit::for_ratio(choice.fit(inset).ratio);
                match display::turn_on(&fit) {
                    Ok(msg) => {
                        println!("[{}] phone connected - display on: {msg}", stamp());
                        start_gestures();
                        if let Err(e) = super::library::show() {
                            println!("[{}] library not shown: {e}", stamp());
                        }
                    }
                    Err(e) => println!("[{}] phone connected but the display failed: {e}", stamp()),
                }
            }
            super::detect::Action::Remove => {
                stop_gestures();
                super::library::hide();
                let back = window::restore_all_placed();
                match display::turn_off() {
                    Ok(msg) => println!("[{}] session ended - {msg}, {back} window(s) home", stamp()),
                    Err(e) => println!("[{}] session ended - restore: {e}", stamp()),
                }
            }
            super::detect::Action::Stay => {}
        }
        if fusion.phase() != last_phase {
            println!("[{}] {}", stamp(), phase_line(&fusion));
            last_phase = fusion.phase();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    // Ctrl-C: leave the desktop as we found it.
    stop_gestures();
    let back = window::restore_all_placed();
    let _ = display::turn_off();
    println!("bye ({back} window(s) home)");
}

fn choice_desc(choice: &super::Choice) -> String {
    match choice {
        super::Choice::Model(m) => format!("model \"{m}\""),
        super::Choice::Size(w, h, _) => format!("screen {w}x{h}"),
        super::Choice::Ratio(r) => format!("ratio {r:.3}"),
    }
}

fn phase_line(fusion: &Fusion) -> String {
    match fusion.phase() {
        Phase::Idle => "idle - no remote session".into(),
        Phase::Active => "remote session, phone display up".into(),
        Phase::Draining => "signals quiet - checking whether the session really ended".into(),
        Phase::Keeping => "session dropped - keeping the display for a short-drop reconnect".into(),
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn stamp() -> String {
    let secs = now_secs() as u64 % 86400;
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

/// The gesture hook runs for the length of the session, on its own thread.
fn start_gestures() {
    #[cfg(windows)]
    std::thread::spawn(|| {
        let Some(area) = crate::phone::window::phone_display_area() else { return };
        let Some(fit) = crate::phone::display::win::held_fit_win() else { return };
        let scale = area.bounds.2 / fit.base_width as f64;
        super::gesture::win::run((area.bounds.0, area.bounds.1, area.bounds.2, area.bounds.3), scale);
    });
}

fn stop_gestures() {
    #[cfg(windows)]
    crate::phone::gesture::win::stop();
}
