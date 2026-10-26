//! Elevation from barometric pressure, anchored to GNSS altitude, with climb
//! and descent counted through a hysteresis band.
//!
//! No other firmware for this board fuses the two (docs/prior-art.md §5), so
//! this is Cairn's own design: pressure is smooth but drifts with weather,
//! GNSS altitude is unbiased but noisy, so the barometer gives the shape and
//! GNSS slowly sets the level. The raw pressure is always recorded too, so the
//! server can redo all of this.

/// Standard sea-level pressure, used until GNSS has anchored the barometer.
pub const STANDARD_QNH_PA: f32 = 101_325.0;

/// Altitude in metres for pressure `p` against sea-level reference `qnh`
/// (international barometric formula, troposphere).
pub fn pressure_altitude_m(p_pa: f32, qnh_pa: f32) -> f32 {
    44_330.0 * (1.0 - libm::powf(p_pa / qnh_pa, 1.0 / 5.255_877))
}

/// The sea-level reference that makes pressure `p` read altitude `h`.
pub fn qnh_for(p_pa: f32, altitude_m: f32) -> f32 {
    p_pa / libm::powf(1.0 - altitude_m / 44_330.0, 5.255_877)
}

/// Settings, with defaults chosen for walking.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// A GNSS altitude is used only if the receiver's vertical accuracy is
    /// better than this.
    pub max_v_acc_m: f32,
    /// Fixes averaged before the first anchor.
    pub anchor_fixes: u16,
    /// After anchoring, each good fix moves the reference this fraction of
    /// the way towards what that fix implies (tracks weather slowly).
    pub drift_gain: f32,
    /// Smoothing applied to barometric altitude before counting climb.
    pub smoothing: f32,
    /// Climb or descent is counted only once altitude leaves this band.
    pub hysteresis_m: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_v_acc_m: 10.0,
            anchor_fixes: 30,
            drift_gain: 0.002,
            smoothing: 0.2,
            hysteresis_m: 3.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub qnh_pa: f32,
    pub anchored: bool,
    /// Smoothed barometric altitude against `qnh_pa`.
    pub elevation_m: f32,
    pub gain_m: f32,
    pub loss_m: f32,
}

/// The fusion state. Feed it every pressure reading and every GNSS fix.
pub struct Elevation {
    cfg: Config,
    qnh_pa: f32,
    anchored: bool,
    anchor_sum: f32,
    anchor_n: u16,
    last_pressure: Option<f32>,
    smoothed: Option<f32>,
    reference: Option<f32>,
    gain_m: f32,
    loss_m: f32,
}

impl Default for Elevation {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Elevation {
    pub const fn new(cfg: Config) -> Self {
        Self {
            cfg,
            qnh_pa: STANDARD_QNH_PA,
            anchored: false,
            anchor_sum: 0.0,
            anchor_n: 0,
            last_pressure: None,
            smoothed: None,
            reference: None,
            gain_m: 0.0,
            loss_m: 0.0,
        }
    }

    pub fn state(&self) -> State {
        State {
            qnh_pa: self.qnh_pa,
            anchored: self.anchored,
            elevation_m: self.smoothed.unwrap_or(0.0),
            gain_m: self.gain_m,
            loss_m: self.loss_m,
        }
    }

    /// A GNSS fix: mean-sea-level altitude and its vertical accuracy, both in
    /// metres. Uses the latest pressure reading.
    pub fn on_gnss(&mut self, hmsl_m: f32, v_acc_m: f32) {
        let Some(p) = self.last_pressure else { return };
        if !(v_acc_m > 0.0 && v_acc_m < self.cfg.max_v_acc_m && hmsl_m.is_finite()) {
            return;
        }
        let implied = qnh_for(p, hmsl_m);
        if !(80_000.0..=120_000.0).contains(&implied) {
            return;
        }
        if !self.anchored {
            self.anchor_sum += implied;
            self.anchor_n += 1;
            if self.anchor_n >= self.cfg.anchor_fixes {
                let old = self.qnh_pa;
                self.qnh_pa = self.anchor_sum / self.anchor_n as f32;
                self.anchored = true;
                self.rebase(old);
            }
        } else {
            let old = self.qnh_pa;
            self.qnh_pa += self.cfg.drift_gain * (implied - self.qnh_pa);
            self.rebase(old);
        }
    }

    /// Shift the smoothed altitude and the climb reference to a new QNH, so
    /// changing the anchor never counts as climbing.
    fn rebase(&mut self, old_qnh: f32) {
        if let Some(p) = self.last_pressure {
            let delta = pressure_altitude_m(p, self.qnh_pa) - pressure_altitude_m(p, old_qnh);
            if let Some(s) = self.smoothed.as_mut() {
                *s += delta;
            }
            if let Some(r) = self.reference.as_mut() {
                *r += delta;
            }
        }
    }

    /// A pressure reading in pascals. Returns the smoothed elevation.
    pub fn on_pressure(&mut self, p_pa: f32) -> f32 {
        if !(30_000.0..=110_000.0).contains(&p_pa) {
            return self.smoothed.unwrap_or(0.0);
        }
        self.last_pressure = Some(p_pa);
        let alt = pressure_altitude_m(p_pa, self.qnh_pa);
        let s = match self.smoothed {
            None => alt,
            Some(prev) => prev + self.cfg.smoothing * (alt - prev),
        };
        self.smoothed = Some(s);
        match self.reference {
            None => self.reference = Some(s),
            Some(r) => {
                if s > r + self.cfg.hysteresis_m {
                    self.gain_m += s - r;
                    self.reference = Some(s);
                } else if s < r - self.cfg.hysteresis_m {
                    self.loss_m += r - s;
                    self.reference = Some(s);
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pressure at altitude `h` for a given QNH.
    fn p_at(h: f32, qnh: f32) -> f32 {
        qnh * libm::powf(1.0 - h / 44_330.0, 5.255_877)
    }

    // A deterministic noise source, so tests do not depend on a crate.
    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        }
    }

    #[test]
    fn formula_round_trips() {
        for h in [-100.0, 0.0, 500.0, 2_500.0, 4_400.0] {
            let p = p_at(h, STANDARD_QNH_PA);
            assert!(
                (pressure_altitude_m(p, STANDARD_QNH_PA) - h).abs() < 0.2,
                "{h}"
            );
            assert!((qnh_for(p, h) - STANDARD_QNH_PA).abs() < 1.0);
        }
    }

    #[test]
    fn flat_and_noisy_counts_no_climb() {
        let mut e = Elevation::default();
        let mut n = Lcg(1);
        for _ in 0..10_000 {
            // ±1 m of altitude noise is about ±12 Pa at sea level.
            e.on_pressure(p_at(200.0 + 2.0 * n.next(), STANDARD_QNH_PA));
        }
        let s = e.state();
        assert!(s.gain_m < 3.0 && s.loss_m < 3.0, "{s:?}");
    }

    #[test]
    fn a_climb_and_a_descent_are_counted() {
        let mut e = Elevation::default();
        let mut n = Lcg(7);
        let mut h = 100.0f32;
        for _ in 0..2_000 {
            h += 0.25;
            e.on_pressure(p_at(h + n.next(), STANDARD_QNH_PA));
        }
        for _ in 0..1_000 {
            h -= 0.25;
            e.on_pressure(p_at(h + n.next(), STANDARD_QNH_PA));
        }
        let s = e.state();
        assert!((s.gain_m - 500.0).abs() < 10.0, "{s:?}");
        assert!((s.loss_m - 250.0).abs() < 10.0, "{s:?}");
    }

    #[test]
    fn gnss_anchors_the_reference_without_counting_a_jump() {
        let true_qnh = 102_000.0;
        let mut e = Elevation::default();
        for _ in 0..40 {
            e.on_pressure(p_at(1_000.0, true_qnh));
            e.on_gnss(1_000.0, 4.0);
        }
        let s = e.state();
        assert!(s.anchored);
        assert!((s.qnh_pa - true_qnh).abs() < 5.0, "{s:?}");
        assert!((s.elevation_m - 1_000.0).abs() < 1.0, "{s:?}");
        assert!(
            s.gain_m < 3.0,
            "re-anchoring must not count as climb: {s:?}"
        );
    }

    #[test]
    fn poor_vertical_accuracy_is_ignored() {
        let mut e = Elevation::default();
        for _ in 0..100 {
            e.on_pressure(p_at(500.0, 100_000.0));
            e.on_gnss(500.0, 25.0);
        }
        assert!(!e.state().anchored);
    }

    #[test]
    fn implausible_pressure_is_ignored() {
        let mut e = Elevation::default();
        e.on_pressure(p_at(0.0, STANDARD_QNH_PA));
        let before = e.state();
        e.on_pressure(0.0);
        e.on_pressure(250_000.0);
        assert_eq!(e.state(), before);
    }
}
