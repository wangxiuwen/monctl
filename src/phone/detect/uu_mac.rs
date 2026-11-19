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
                // Being controlled: the UURemoteServer overlay on screen or the
        // server process running. v1 skips the AX controller-table read
        // (needs Accessibility), so the signal is Unknown and Fusion
        // treats it as a phone after the grace period.
        if crate::phone::detect::probe_mac::owner_on_screen("UURemote")
            || crate::phone::detect::probe_mac::process_running("UURemoteServer")
        {
            vec![Signal { app: "uu", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
