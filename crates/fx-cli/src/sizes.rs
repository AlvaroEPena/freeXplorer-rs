use crate::CliError;

/// "64M", "8G", "1.5T", "512KB" or plain bytes.
pub fn parse(text: &str) -> Result<u64, CliError> {
    let bad = || {
        CliError::Usage(format!(
            "\"{}\" isn't a size. Use something like 64M, 8G or 1.5T.",
            text.trim()
        ))
    };

    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut digits = compact.as_str();
    let mut scale = 1u64;
    let unit = |c: char| match c.to_ascii_uppercase() {
        'K' => Some(1u64 << 10),
        'M' => Some(1 << 20),
        'G' => Some(1 << 30),
        'T' => Some(1 << 40),
        _ => None,
    };

    if let Some(last) = digits.chars().last() {
        if let Some(s) = unit(last) {
            scale = s;
            digits = &digits[..digits.len() - 1];
        } else if last.eq_ignore_ascii_case(&'B') {
            digits = &digits[..digits.len() - 1];
            if let Some(s) = digits.chars().last().and_then(unit) {
                scale = s;
                digits = &digits[..digits.len() - 1];
            }
        }
    }
    let value: f64 = digits.parse().map_err(|_| bad())?;
    if !value.is_finite() || value <= 0.0 {
        return Err(bad());
    }
    Ok((value * scale as f64) as u64)
}

/// A cluster size as a number of 512 byte sectors.
pub fn cluster_sectors(text: &str) -> Result<u32, CliError> {
    let bytes = parse(text)?;
    if !(4096..=65536).contains(&bytes) || !bytes.is_power_of_two() {
        return Err(CliError::Usage("cluster size has to be 4K, 8K, 16K, 32K or 64K".into()));
    }
    Ok((bytes / 512) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_with_units() {
        assert_eq!(parse("64M").unwrap(), 64 << 20);
        assert_eq!(parse("8g").unwrap(), 8 << 30);
        assert_eq!(parse("1.5T").unwrap(), 3 << 39);
        assert_eq!(parse("512 KB").unwrap(), 512 << 10);
        assert_eq!(parse("1000").unwrap(), 1000);
        assert_eq!(parse("10B").unwrap(), 10);
    }

    #[test]
    fn nonsense_is_refused() {
        for text in ["", "G", "-4G", "0", "abc", "1.2.3G"] {
            assert!(parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn cluster_sizes() {
        assert_eq!(cluster_sectors("16K").unwrap(), 32);
        assert_eq!(cluster_sectors("4096").unwrap(), 8);
        assert!(cluster_sectors("3K").is_err());
        assert!(cluster_sectors("128K").is_err());
        assert!(cluster_sectors("2K").is_err());
    }
}
