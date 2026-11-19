// ToDesk on macOS. Signals need calibration on a real session.
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
                // Experimental: needs calibration against a real controlled session.
        if crate::phone::detect::probe_mac::owner_on_screen("ToDesk")
            || crate::phone::detect::probe_mac::process_running("ToDesk")
        {
            vec![Signal { app: "todesk", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
