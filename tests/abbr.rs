//! Port of `tests/test_abbr.py`.

use afi::metrics::{abbr, precise_abbr};

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
