// The phone display's proportions and resolution ladder.
//
// A remote-control app fits its stream to the phone with black bars, its
// toolbar floating over the picture. The phone display therefore gets the
// phone's aspect ratio minus that toolbar room, so the picture lands exactly
// between the toolbar and the notch. Width follows what is on screen:
// narrow (540pt) while the home page shows - the biggest text - and wider
// only when a window's own minimum size demands it, in equal-ratio steps so
// the stream never restarts and taps stay accurate.
//
// The math follows Snug (github.com/zephyr4123/snug, MIT); the toolbar inset
// is per remote app (91pt is measured on UU Remote on an iPhone 17 Pro Max).

/// Snug's own panels and gesture thresholds are designed at this width; the
/// scale factor keeps everything finger-sized on wider steps.
pub const DESIGN_WIDTH: u32 = 540;
/// 2x HiDPI becomes desktop-usable only from here (macOS, measured):
/// width >= 540pt and height >= 800pt. Shorter phones widen their base step.
pub const MIN_WIDTH: u32 = 540;
pub const MIN_HEIGHT: u32 = 800;
/// Widest step: text is half the base size, no larger than reading the main
/// display through the remote app - wider steps buy nothing.
pub const MAX_WIDTH: u32 = 1080;
/// Step spacing: after fitting a window we overshoot by at most ~2%.
pub const STEP: u32 = 10;
/// Squarest step (foldables) to tallest (up to 2:1).
pub const RATIO_MIN: f64 = 1.0;
pub const RATIO_MAX: f64 = 2.0;
/// Pixel ceiling framing every ratio at the widest step, 2x included; one
/// identity keeps all its steps, so switching ratios keeps ICC files down.
pub const MAX_PIXELS: (u32, u32) = (MAX_WIDTH * 2, (MAX_WIDTH as f64 * RATIO_MAX) as u32 * 2);
/// Toolbar room per side (top toolbar/notch, bottom toolbar), in points.
pub const DEFAULT_INSET: f64 = 91.0;
/// Android phones have no public logical width; assume a common 400dp.
pub const ASSUMED_POINTS: f64 = 400.0;
/// The home indicator bar, at the base width.
pub const BASE_HOME_BAR: f64 = 28.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    /// Points per point: display height / width at any step.
    pub ratio: f64,
    /// The base (narrowest) step; the home page and fitting windows use it.
    pub base_width: u32,
    /// Registered steps, base to widest, same aspect ratio throughout.
    pub widths: Vec<u32>,
}

impl Fit {
    /// From the phone's physical screen (portrait pixels) and logical width.
    /// `inset` removes the remote app's toolbar room from the ratio.
    pub fn for_screen(w: u32, h: u32, points: Option<u32>, inset: f64) -> Fit {
        let (w, h) = portrait(w, h);
        let points = points.map(|p| p as f64).unwrap_or(ASSUMED_POINTS).max(1.0);
        Fit::for_ratio(h as f64 / w as f64 - 2.0 * inset / points)
    }

    pub fn for_ratio(ratio: f64) -> Fit {
        let ratio = ratio.clamp(RATIO_MIN, RATIO_MAX);
        // A short screen has no 2x step at MIN_WIDTH: widen the base until it
        // is tall enough (a ratio of >= 1 always gets there at 800 wide).
        let base_width = (MIN_WIDTH..=MAX_WIDTH)
            .step_by(STEP as usize)
            .find(|w| (f64::from(*w) * ratio).round() >= f64::from(MIN_HEIGHT))
            .unwrap_or(MAX_WIDTH);
        let widths: Vec<u32> = (base_width..=MAX_WIDTH).step_by(STEP as usize).collect();
        Fit { ratio, base_width, widths }
    }

    /// Logical size (points) of one step.
    pub fn size(&self, width: u32) -> (u32, u32) {
        (width, (f64::from(width) * self.ratio).round() as u32)
    }

    /// Pixels of one step at 2x.
    pub fn pixels(&self, width: u32) -> (u32, u32) {
        let (w, h) = self.size(width);
        (w * 2, h * 2)
    }

    /// Which step fits a window. `window` is the size read back after the
    /// window was filled into `area` (the current step's usable area): if it
    /// fits, stay; if the app would not shrink that far, take the narrowest
    /// step that holds it, at most the widest.
    pub fn width_to_fit(&self, window: (f64, f64), area: (f64, f64), current: u32) -> u32 {
        let tolerance = 2.0;
        if window.0 <= area.0 + tolerance && window.1 <= area.1 + tolerance {
            return current;
        }
        let base = f64::from(current);
        // Multiply before dividing: floats otherwise land a hair above the
        // boundary and round up into the next step.
        let needed = (base * window.0 / area.0).max(base * window.1 / area.1);
        self.widths.iter().copied().find(|w| f64::from(*w) >= needed.round().ceil()).unwrap_or(MAX_WIDTH)
    }

    /// How many times smaller than the design width a step is: Snug-style
    /// panels and gesture thresholds multiply by this to keep their on-phone
    /// size.
    pub fn scale(screen_width: f64) -> f64 {
        screen_width / f64::from(DESIGN_WIDTH)
    }

    /// The home indicator bar at a given step; windows on the display leave
    /// this strip free.
    pub fn home_bar_height(screen_width: f64) -> u32 {
        (BASE_HOME_BAR * Self::scale(screen_width)).round() as u32
    }

    pub fn describe(&self) -> String {
        let (w, h) = self.size(self.base_width);
        format!("{}x{}", w, h)
    }
}

/// Portrait orientation; landscape input is rotated (only the ratio matters).
pub fn portrait(w: u32, h: u32) -> (u32, u32) {
    if w > h {
        (h, w)
    } else {
        (w, h)
    }
}

// ---- CLI ----------------------------------------------------------------

pub fn run_cli(args: &[String]) {
    let mut inset = DEFAULT_INSET;
    let mut ratio: Option<f64> = None;
    let mut size: Option<(u32, u32)> = None;
    let mut points: Option<u32> = None;
    let mut query: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--inset" => inset = it.next().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_INSET),
            "--ratio" => ratio = it.next().and_then(|v| v.parse().ok()),
            "--size" => size = it.next().and_then(|v| parse_size(v)),
            "--points" => points = it.next().and_then(|v| v.parse().ok()),
            other => query.push(other),
        }
    }

    let fit = if let Some((w, h)) = size {
        Fit::for_screen(w, h, points, inset)
    } else if let Some(r) = ratio {
        Fit::for_ratio(r)
    } else {
        match super::models::lookup(&query.join(" ")) {
            Some((brand, m)) => {
                println!("{} {} ({}x{}, {})", brand.name, m.name, m.screen.0, m.screen.1, match m.points {
                    Some(p) => format!("{}pt", p),
                    None => "no logical width, assuming 400pt".into(),
                });
                Fit::for_screen(m.screen.0, m.screen.1, m.points, inset)
            }
            None => {
                crate::common::fail("no phone given: `monctl phone fit <model query | --size WxH | --ratio R> [--inset N] [--points PT]`");
            }
        }
    };
    println!("fit     {} (ratio {:.3}, {} steps to {} wide)", fit.describe(), fit.ratio, fit.widths.len(), MAX_WIDTH);
    println!("pixels  base {}x{}, max {}x{}", fit.pixels(fit.base_width).0, fit.pixels(fit.base_width).1, fit.pixels(MAX_WIDTH).0, fit.pixels(MAX_WIDTH).1);
    println!("inset   {}pt per side (toolbar room)", inset);
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', '*'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iphone_17_pro_max_lands_on_540x950() {
        // Snug's tuned reference: 1320x2868 at 440pt with a 91pt inset.
        let fit = Fit::for_screen(1320, 2868, Some(440), DEFAULT_INSET);
        assert_eq!(fit.size(540), (540, 950));
        assert_eq!(fit.base_width, 540);
        assert_eq!(fit.widths.first(), Some(&540));
        assert_eq!(fit.widths.last(), Some(&MAX_WIDTH));
    }

    #[test]
    fn short_screens_widen_the_base_for_2x() {
        // iPhone 8: 750x1334 at 375pt; 540 wide is too short for HiDPI.
        let fit = Fit::for_screen(750, 1334, Some(375), DEFAULT_INSET);
        assert_eq!(fit.base_width, 620);
        assert_eq!(fit.widths.first(), Some(&620));
    }

    #[test]
    fn ratios_are_clamped() {
        // Foldable inner screen minus the toolbar goes squarer than square.
        assert_eq!(Fit::for_ratio(0.5).ratio, RATIO_MIN);
        assert_eq!(Fit::for_ratio(4.0).ratio, RATIO_MAX);
        assert_eq!(Fit::for_ratio(1.759).base_width, 540);
    }

    #[test]
    fn landscape_input_is_rotated() {
        assert_eq!(Fit::for_screen(2868, 1320, Some(440), DEFAULT_INSET), Fit::for_screen(1320, 2868, Some(440), DEFAULT_INSET));
    }

    #[test]
    fn android_without_points_assumes_400() {
        // 1080x2400 at 400pt: 2400/1080 - 182/400 = 1.767
        let fit = Fit::for_screen(1080, 2400, None, DEFAULT_INSET);
        assert_eq!(fit.size(540), (540, 954));
    }

    #[test]
    fn ladder_steps_stay_near_equal_ratio_so_streams_never_restart() {
        // Each step's height is rounded to whole points, so the ratio wobbles
        // by far less than the ~0.1% a remote app's fitting tolerates.
        let fit = Fit::for_ratio(1.759);
        let (w0, h0) = fit.size(540);
        for w in [550, 700, 900, MAX_WIDTH] {
            let (w1, h1) = fit.size(w);
            assert!((h1 as f64 / w1 as f64 - h0 as f64 / w0 as f64).abs() < 0.002, "{}x{} drifted", w1, h1);
        }
    }

    #[test]
    fn width_to_fit_stays_when_it_fits() {
        let fit = Fit::for_ratio(1.759);
        let (w, h) = fit.size(540);
        let area = (f64::from(w), f64::from(h) - f64::from(Fit::home_bar_height(540.0)));
        assert_eq!(fit.width_to_fit(area, area, 540), 540);
    }

    #[test]
    fn width_to_fit_takes_the_narrowest_step_that_holds() {
        let fit = Fit::for_ratio(1.759);
        // A window stuck at 1.5x the base area needs 810; the ladder has a
        // step at exactly 810, so that is the narrowest that holds it.
        let needed = (f64::from(540) * 1.5, f64::from(950) * 1.5);
        let area = (540.0, 950.0);
        assert_eq!(fit.width_to_fit(needed, area, 540), 810);
    }

    #[test]
    fn floats_landing_a_hair_over_do_not_step_up() {
        let fit = Fit::for_ratio(1.759);
        // 540 * (580 / 540) is 580.0000000001 in floats: must stay on 580.
        let area = (580.0, 1020.0);
        assert_eq!(fit.width_to_fit((580.0, 1020.0), area, 580), 580);
    }

    #[test]
    fn scale_and_home_bar_track_the_step() {
        assert_eq!(Fit::scale(540.0), 1.0);
        assert_eq!(Fit::home_bar_height(540.0), 28);
        assert_eq!(Fit::home_bar_height(1080.0), 56);
    }
}
