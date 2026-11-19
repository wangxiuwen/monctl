// Sunlogin (向日葵) on Windows. Signals need calibration on a real session.
use super::Signal;

pub struct SunloginWin;

impl super::Detector for SunloginWin {
    fn name(&self) -> &'static str {
        "sunlogin"
    }

    fn inset(&self) -> f64 {
        0.0
    }

    fn poll(&mut self) -> Vec<Signal> {
                // Untested against a real controlled session; same reasoning as
        // ToDesk - the idle client keeps windows and connections up, so
        // only the controlled badge counts.
        if crate::phone::detect::probe_win::window_present("sunlogin", "被控")
            || crate::phone::detect::probe_win::window_present("sunlogin", "正在控制")
        {
            vec![Signal { app: "sunlogin", device: super::Device::Unknown }]
        } else {
            Vec::new()
        }
    }
}
