//! Rate limits shared by the normal controller and optional calibration code.

/// Maximum accepted configured or measured link rate, in kbit/s.
pub const MAX_RATE_KBPS: u64 = 100_000_000;

pub const DEFAULT_MIN_DL_SHAPER_RATE_KBPS: u64 = 5_000;
pub const DEFAULT_BASE_DL_SHAPER_RATE_KBPS: u64 = 20_000;
pub const DEFAULT_MAX_DL_SHAPER_RATE_KBPS: u64 = 80_000;
pub const DEFAULT_MIN_UL_SHAPER_RATE_KBPS: u64 = 5_000;
pub const DEFAULT_BASE_UL_SHAPER_RATE_KBPS: u64 = 20_000;
pub const DEFAULT_MAX_UL_SHAPER_RATE_KBPS: u64 = 35_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_rate_covers_current_openwrt_link_classes() {
        assert_eq!(MAX_RATE_KBPS, 100_000_000);
    }

    #[test]
    fn controller_defaults_are_ordered_and_bounded() {
        assert!(DEFAULT_MIN_DL_SHAPER_RATE_KBPS <= DEFAULT_BASE_DL_SHAPER_RATE_KBPS);
        assert!(DEFAULT_BASE_DL_SHAPER_RATE_KBPS <= DEFAULT_MAX_DL_SHAPER_RATE_KBPS);
        assert!(DEFAULT_MIN_UL_SHAPER_RATE_KBPS <= DEFAULT_BASE_UL_SHAPER_RATE_KBPS);
        assert!(DEFAULT_BASE_UL_SHAPER_RATE_KBPS <= DEFAULT_MAX_UL_SHAPER_RATE_KBPS);
        assert!(DEFAULT_MAX_DL_SHAPER_RATE_KBPS <= MAX_RATE_KBPS);
        assert!(DEFAULT_MAX_UL_SHAPER_RATE_KBPS <= MAX_RATE_KBPS);
    }
}
