// lib entry: shared by the monctl CLI (src/main.rs) and the Windows tray
// (src/bin/monctl-tray.rs).

pub mod awake;
pub mod common;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
