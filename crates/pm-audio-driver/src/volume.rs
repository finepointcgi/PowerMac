//! The output volume control's curve: scalar 0–1 (the slider) ↔ decibels
//! ↔ linear gain. `dB = 40·log10(scalar)`, so the gain is `scalar²` — a
//! gentle audio taper — and the range is −96 dB…0 dB, with 0 meaning
//! silence.

pub const MIN_DB: f32 = -96.0;
pub const MAX_DB: f32 = 0.0;

pub fn scalar_to_db(scalar: f32) -> f32 {
    if scalar <= 0.0 {
        return MIN_DB;
    }
    (40.0 * scalar.min(1.0).log10()).clamp(MIN_DB, MAX_DB)
}

pub fn db_to_scalar(db: f32) -> f32 {
    if db <= MIN_DB {
        return 0.0;
    }
    10f32.powf(db.min(MAX_DB) / 40.0).clamp(0.0, 1.0)
}

/// Linear gain applied to samples.
pub fn gain(scalar: f32, muted: bool) -> f32 {
    if muted || scalar <= 0.0 {
        0.0
    } else {
        let s = scalar.min(1.0);
        s * s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_points() {
        assert_eq!(scalar_to_db(1.0), 0.0);
        assert_eq!(scalar_to_db(0.0), MIN_DB);
        assert_eq!(db_to_scalar(0.0), 1.0);
        assert_eq!(db_to_scalar(MIN_DB), 0.0);
        assert_eq!(gain(1.0, false), 1.0);
        assert_eq!(gain(1.0, true), 0.0);
        assert_eq!(gain(0.0, false), 0.0);
    }

    #[test]
    fn round_trips_and_matches_gain() {
        for i in 1..=100 {
            let s = i as f32 / 100.0;
            let db = scalar_to_db(s);
            if db > MIN_DB {
                assert!((db_to_scalar(db) - s).abs() < 1e-4, "{s}");
                // gain in dB equals the control's dB value.
                assert!((20.0 * gain(s, false).log10() - db).abs() < 1e-3, "{s}");
            }
        }
        assert!((scalar_to_db(0.5) - (-12.041)).abs() < 1e-2);
    }

    #[test]
    fn clamps() {
        assert_eq!(scalar_to_db(2.0), 0.0);
        assert_eq!(db_to_scalar(12.0), 1.0);
        assert_eq!(db_to_scalar(-200.0), 0.0);
    }
}
