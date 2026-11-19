// ToDesk on macOS.
//
// Disabled until calibrated: the client is resident on machines that have
// it, so process and window probes fire while idle - false phone sessions.
// Calibrate with `monctl phone detect` during a real controlled session
// before re-enabling.
use super::Signal;

pub struct TodeskMac;

impl super::Detector for TodeskMac {
    fn name(&self) -> &'static str {
        "todesk"
    }

    fn inset(&self) -> f64 {
        0.0
    }

    fn poll(&mut self) -> Vec<Signal> {
        Vec::new()
    }
}
