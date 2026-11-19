// UU Remote / GameViewer on Windows (snug-win's signal set, minus UIA).
use super::Signal;

pub struct UuWin;

impl super::Detector for UuWin {
    fn name(&self) -> &'static str {
        "uu"
    }

    fn inset(&self) -> f64 {
        super::super::fit::DEFAULT_INSET
    }

    fn poll(&mut self) -> Vec<Signal> {
                // Calibrated: GameViewerServer writes streamer_log_controlled_*.slog
        // only while a controller streams this machine. The controller's
        // device type would need UIA; Unknown makes Fusion treat it as a
        // phone after the grace period, which is the main use case.
        if crate::phone::detect::probe_win::uu_controlled() {
            vec![Signal { app: "uu", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
