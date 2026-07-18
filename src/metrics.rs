//! Token-count abbreviators for the stats footer.
//!
//! `_abbr` keeps the footer tidy once context grows; `_precise_abbr` keeps
//! two decimals for the live tool-call-args counter where watching the number
//! tick up is part of the feedback.

/// Compact token count: 832 -> "832", 1500 -> "1.5K", 78825 -> "78K",
/// 1234567 -> "1.2M".
pub fn abbr(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 10_000 {
        return format!("{:.1}K", n as f64 / 1000.0);
    }
    if n < 1_000_000 {
        return format!("{}K", n / 1000);
    }
    if n < 10_000_000 {
        return format!("{:.1}M", n as f64 / 1_000_000.0);
    }
    format!("{}M", n / 1_000_000)
}

/// Like `abbr` but keeps two decimals: 832 -> "832", 25152 -> "25.15K",
/// 1234567 -> "1.23M", 1_500_000_000 -> "1.50B".
pub fn precise_abbr(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        return format!("{:.2}K", n as f64 / 1000.0);
    }
    if n < 1_000_000_000 {
        return format!("{:.2}M", n as f64 / 1_000_000.0);
    }
    format!("{:.2}B", n as f64 / 1_000_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbr_ranges() {
        assert_eq!(abbr(832), "832");
        assert_eq!(abbr(1500), "1.5K");
        assert_eq!(abbr(78825), "78K");
        assert_eq!(abbr(1234567), "1.2M");
    }

    #[test]
    fn precise_abbr_keeps_two_decimals() {
        assert_eq!(precise_abbr(832), "832");
        assert_eq!(precise_abbr(25152), "25.15K");
        assert_eq!(precise_abbr(1234567), "1.23M");
    }

    #[test]
    fn precise_abbr_billions_two_decimals() {
        assert_eq!(precise_abbr(1_500_000_000), "1.50B");
        assert_eq!(precise_abbr(2_000_000_000), "2.00B");
    }
}
