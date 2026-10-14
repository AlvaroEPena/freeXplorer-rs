use super::geometry::MAX_FILE_NAME_LENGTH;
use crate::error::{Error, ErrorKind, Result};

pub const MAX_LENGTH: usize = MAX_FILE_NAME_LENGTH;
pub const MAX_PATH_LENGTH: usize = 240;

const PUNCTUATION: &str = " !#$%&'()-.@[]^_`{}~";

pub fn is_valid_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || PUNCTUATION.contains(c)
}

pub fn is_valid(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > MAX_LENGTH {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    name.chars().all(is_valid_char)
}

pub fn require(name: &str) -> Result<()> {
    if name.chars().count() > MAX_LENGTH {
        return Err(Error::fatx(
            ErrorKind::InvalidName,
            format!("\"{name}\" is longer than {MAX_LENGTH} characters"),
        ));
    }
    if !is_valid(name) {
        return Err(Error::fatx(
            ErrorKind::InvalidName,
            format!("\"{name}\" has characters FATX can't store"),
        ));
    }
    Ok(())
}

/// Splits on either slash and drops the empty pieces.
pub fn split_path(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|part| !part.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_pass() {
        for name in ["Content", "0000000000000000", "my save-1.bin", "A (copy)", "x"] {
            assert!(is_valid(name), "{name}");
        }
    }

    #[test]
    fn what_fatx_cant_store_is_refused() {
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "tab\there",
            "caf\u{e9}",
        ] {
            assert!(!is_valid(name), "{name:?}");
        }
        assert!(!is_valid(&"x".repeat(43)));
        assert!(is_valid(&"x".repeat(42)));
    }

    #[test]
    fn require_says_why() {
        assert!(require(&"x".repeat(43)).unwrap_err().to_string().contains("longer"));
        assert!(require("a:b").unwrap_err().to_string().contains("can't store"));
    }

    #[test]
    fn paths_split_on_both_slashes() {
        assert_eq!(split_path("\\Content/A\\\\B"), vec!["Content", "A", "B"]);
        assert!(split_path("\\").is_empty());
    }
}
