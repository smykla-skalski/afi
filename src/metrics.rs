//! Token-count abbreviators for the stats footer.
//!
//! `_abbr` keeps the footer tidy once context grows; `_precise_abbr` keeps
//! two decimals for the live tool-call-args counter where watching the number
//! tick up is part of the feedback.
//!
//! The decimals use integer round-half-up math (`scaled_round`) rather than
//! float division so there is no `u64 -> f64` precision loss; the results match
//! the previous `{:.1}`/`{:.2}` formatting for every in-range value.

/// Round `n / divisor` to a fixed number of places, returning the value scaled
/// by `scale` (`10^places`). Integer round-half-up, computed in `u128` so the
/// intermediate never overflows and no `as` cast is needed.
fn scaled_round(n: u64, divisor: u64, scale: u64) -> u64 {
    let numerator = u128::from(n) * u128::from(scale) + u128::from(divisor) / 2;
    let scaled = numerator / u128::from(divisor);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// Compact token count: 832 -> "832", 1500 -> "1.5K", 78825 -> "78K",
/// 1234567 -> "1.2M".
#[must_use]
pub fn abbr(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 10_000 {
        let tenths = scaled_round(n, 1000, 10);
        return format!("{}.{}K", tenths / 10, tenths % 10);
    }
    if n < 1_000_000 {
        return format!("{}K", n / 1000);
    }
    if n < 10_000_000 {
        let tenths = scaled_round(n, 1_000_000, 10);
        return format!("{}.{}M", tenths / 10, tenths % 10);
    }
    format!("{}M", n / 1_000_000)
}

/// Like `abbr` but keeps two decimals: 832 -> "832", 25152 -> "25.15K",
/// 1234567 -> "1.23M", `1_500_000_000` -> "1.50B".
#[must_use]
pub fn precise_abbr(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        let hundredths = scaled_round(n, 1000, 100);
        return format!("{}.{:02}K", hundredths / 100, hundredths % 100);
    }
    if n < 1_000_000_000 {
        let hundredths = scaled_round(n, 1_000_000, 100);
        return format!("{}.{:02}M", hundredths / 100, hundredths % 100);
    }
    let hundredths = scaled_round(n, 1_000_000_000, 100);
    format!("{}.{:02}B", hundredths / 100, hundredths % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbr_ranges() {
        assert_eq!(abbr(832), "832");
        assert_eq!(abbr(1500), "1.5K");
        assert_eq!(abbr(78_825), "78K");
        assert_eq!(abbr(1_234_567), "1.2M");
    }

    #[test]
    fn precise_abbr_keeps_two_decimals() {
        assert_eq!(precise_abbr(832), "832");
        assert_eq!(precise_abbr(25_152), "25.15K");
        assert_eq!(precise_abbr(1_234_567), "1.23M");
    }

    #[test]
    fn precise_abbr_billions_two_decimals() {
        assert_eq!(precise_abbr(1_500_000_000), "1.50B");
        assert_eq!(precise_abbr(2_000_000_000), "2.00B");
    }
}
