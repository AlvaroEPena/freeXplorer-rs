/// "111.8 GB", "2 GB", "512 B". One decimal at most, and a trailing .0 is dropped.
pub fn format(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }

    // values like 1.25 sit exactly between two tenths, those round up
    let quarters = value * 4.0;
    if quarters.fract() == 0.0 && (quarters as u64) % 2 == 1 {
        value += 1e-9;
    }
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn small_numbers_stay_in_bytes() {
        assert_eq!(format(0), "0 B");
        assert_eq!(format(1023), "1023 B");
    }

    #[test]
    fn drops_the_decimal_when_it_is_zero() {
        assert_eq!(format(1 << 20), "1 MB");
        assert_eq!(format(2 << 30), "2 GB");
    }

    #[test]
    fn keeps_one_decimal() {
        assert_eq!(format(120_034_123_776), "111.8 GB");
        assert_eq!(format(2_361_393_152), "2.2 GB");
    }

    #[test]
    fn halfway_values_round_up() {
        assert_eq!(format(1280), "1.3 KB");
        assert_eq!(format(1792), "1.8 KB");
    }
}
