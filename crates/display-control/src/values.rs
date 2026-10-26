//! Value scaling between percentages and raw monitor units.

/// Percent (0–100) → raw value on a `0..=max` scale (rounded).
pub fn pct_to_raw(pct: u8, max: u16) -> u16 {
    let max = effective_max(max) as u32;
    ((pct.min(100) as u32 * max + 50) / 100) as u16
}

/// Raw value → percent (rounded, clamped to 100).
pub fn raw_to_pct(raw: u16, max: u16) -> u8 {
    let max = effective_max(max) as u32;
    (((raw as u32).min(max) * 100 + max / 2) / max) as u8
}

/// A reported maximum of 0 is treated as 100 (MCCS' usual scale).
pub fn effective_max(max: u16) -> u16 {
    if max == 0 {
        100
    } else {
        max
    }
}

/// Lowest gamma multiplier software dimming goes to, so the screen stays
/// readable at 0 %.
pub const MIN_DIM_FACTOR: f32 = 0.15;

/// Gamma multiplier for a software-dimming level in percent.
pub fn dim_factor(pct: u8) -> f32 {
    MIN_DIM_FACTOR + (1.0 - MIN_DIM_FACTOR) * pct.min(100) as f32 / 100.0
}

/// Native (DisplayServices / IOCTL) brightness in 0.0–1.0 → percent.
pub fn level_to_pct(level: f32) -> u8 {
    if !level.is_finite() {
        return 0;
    }
    (level.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_roundtrips() {
        assert_eq!(pct_to_raw(50, 100), 50);
        assert_eq!(pct_to_raw(100, 100), 100);
        assert_eq!(pct_to_raw(0, 100), 0);
        assert_eq!(pct_to_raw(50, 0), 50, "max 0 → 100");
        assert_eq!(pct_to_raw(33, 255), 84);
        assert_eq!(pct_to_raw(200, 100), 100, "clamped");
        assert_eq!(pct_to_raw(100, 0xFFFF), 0xFFFF);
        assert_eq!(raw_to_pct(84, 255), 33);
        assert_eq!(raw_to_pct(255, 255), 100);
        assert_eq!(raw_to_pct(300, 255), 100);
        assert_eq!(raw_to_pct(7, 0), 7);
        assert_eq!(raw_to_pct(1, 1), 100);
        assert_eq!(raw_to_pct(0, 1), 0);
        for max in [1u16, 7, 50, 100, 255, 1000, 0xFFFF] {
            for pct in 0..=100u8 {
                let raw = pct_to_raw(pct, max);
                assert!(raw <= max);
                if max >= 100 {
                    assert_eq!(raw_to_pct(raw, max), pct, "max {max} pct {pct}");
                }
            }
        }
    }

    #[test]
    fn dimming_and_levels() {
        assert_eq!(dim_factor(100), 1.0);
        assert!((dim_factor(0) - MIN_DIM_FACTOR).abs() < 1e-6);
        assert!(dim_factor(50) > dim_factor(49));
        assert_eq!(level_to_pct(0.504), 50);
        assert_eq!(level_to_pct(1.5), 100);
        assert_eq!(level_to_pct(-1.0), 0);
        assert_eq!(level_to_pct(f32::NAN), 0);
    }
}
