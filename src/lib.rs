// lib entry: shared by the monctl CLI (src/main.rs) and the Windows tray
// (src/bin/monctl-tray.rs).

pub mod awake;
#[cfg(any(target_os = "macos", windows))]
pub mod cast;
pub mod common;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod qr;
#[cfg(target_os = "macos")]
pub mod virt;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub mod winvdd;
