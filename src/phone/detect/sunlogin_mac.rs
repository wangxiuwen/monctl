// Sunlogin (向日葵) on macOS.
//
// Disabled until calibrated (same reason as ToDesk: the resident client
// makes idle probes fire).
use super::Signal;

pub struct SunloginMac;

impl super::Detector for SunloginMac {
    fn name(&self) -> &'static str {
        "sunlogin"
    }

    fn inset(&self) -> f64 {
        0.0
    }

    fn poll(&mut self) -> Vec<Signal> {
        Vec::new()
    }
}
