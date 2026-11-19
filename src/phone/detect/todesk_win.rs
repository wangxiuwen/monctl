// ToDesk on Windows. Signals need calibration on a real session.
use super::Signal;

pub struct TodeskWin;

impl super::Detector for TodeskWin {
    fn name(&self) -> &'static str {
        "todesk"
    }

    fn inset(&self) -> f64 {
        0.0
    }

    fn poll(&mut self) -> Vec<Signal> {
                // Untested against a real controlled session (ToDesk's Logs dir was
        // empty to calibrate from): the controlled badge window is the
        // signal candidate. The client idles with windows and relay
        // connections up, so presence and connectivity prove nothing.
        if crate::phone::detect::probe_win::window_present("todesk", "被控")
            || crate::phone::detect::probe_win::window_present("todesk", "正在控制")
        {
            vec![Signal { app: "todesk", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
