// UU Remote on macOS (Snug's signal set, minus the AX controller table).
use super::Signal;

pub struct UuMac;

impl super::Detector for UuMac {
    fn name(&self) -> &'static str {
        "uu"
    }

    fn inset(&self) -> f64 {
        super::super::fit::DEFAULT_INSET
    }

    fn poll(&mut self) -> Vec<Signal> {
                // Being controlled: the UURemoteServer process is resident even when
        // idle, so process presence proves nothing. The controlled-session
        // overlay window on screen is the signal (same one Snug uses).
        // Marked experimental until calibrated against a real phone session:
        // run `monctl phone detect` while controlled to check it fires.
        if crate::phone::detect::probe_mac::owner_on_screen("UURemoteServer")
            || crate::phone::detect::probe_mac::owner_on_screen("UU远程")
        {
            vec![Signal { app: "uu", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
