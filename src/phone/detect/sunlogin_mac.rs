// Sunlogin (向日葵) on macOS. Signals need calibration on a real session.
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
                // Experimental: needs calibration against a real controlled session.
        if crate::phone::detect::probe_mac::owner_on_screen("Sunlogin")
            || crate::phone::detect::probe_mac::process_running("SunloginClient")
        {
            vec![Signal { app: "sunlogin", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
