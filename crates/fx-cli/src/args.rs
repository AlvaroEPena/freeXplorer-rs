use std::collections::{HashMap, HashSet};

use crate::CliError;

/// Tiny argument parser: positional words, --name value, --flag, --name=value and a few -x shortcuts.
/// The positional words stay loose because "[partition] <path>" can't be told apart without
/// looking at the drive.
#[derive(Debug, Default)]
pub struct Args {
    options: HashMap<String, Option<String>>,
    pub positional: Vec<String>,
}

const SHORTCUTS: [(char, &str); 5] = [
    ('l', "long"),
    ('r', "recursive"),
    ('f', "force"),
    ('y', "yes"),
    ('w', "write"),
];

impl Args {
    pub fn parse(args: &[String], options_with_values: &[&str]) -> Result<Args, CliError> {
        let takes_value: HashSet<&str> = options_with_values.iter().copied().collect();
        let mut parsed = Args::default();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--" {
                parsed.positional.extend(args[i + 1..].iter().cloned());
                break;
            }
            if let Some(body) = a.strip_prefix("--").filter(|b| !b.is_empty()) {
                if let Some((name, value)) = body.split_once('=') {
                    parsed.options.insert(name.to_lowercase(), Some(value.to_string()));
                } else if takes_value.contains(body.to_lowercase().as_str()) {
                    i += 1;
                    let value = args
                        .get(i)
                        .ok_or_else(|| CliError::Usage(format!("--{body} needs a value")))?;
                    parsed.options.insert(body.to_lowercase(), Some(value.clone()));
                } else {
                    parsed.options.insert(body.to_lowercase(), None);
                }
            } else if a.len() > 1 && a.starts_with('-') && !a[1..].starts_with(|c: char| c.is_ascii_digit()) {
                for c in a[1..].chars() {
                    let (_, name) = SHORTCUTS
                        .iter()
                        .find(|(short, _)| *short == c)
                        .ok_or_else(|| CliError::Usage(format!("unknown option -{c}")))?;
                    parsed.options.insert((*name).to_string(), None);
                }
            } else {
                parsed.positional.push(a.clone());
            }
            i += 1;
        }
        Ok(parsed)
    }

    pub fn has(&self, name: &str) -> bool {
        self.options.contains_key(name)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.options.get(name).and_then(|v| v.as_deref())
    }

    pub fn require(&self, index: usize, what: &str) -> Result<&str, CliError> {
        self.positional
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| CliError::Usage(format!("missing {what}")))
    }

    pub fn reject_unknown(&self, known: &[&str]) -> Result<(), CliError> {
        let mut unknown: Vec<&String> = self.options.keys().filter(|k| !known.contains(&k.as_str())).collect();
        unknown.sort();
        match unknown.first() {
            Some(key) => Err(CliError::Usage(format!("unknown option --{key} for this command"))),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(items: &[&str], with_values: &[&str]) -> Args {
        let items: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        Args::parse(&items, with_values).unwrap()
    }

    #[test]
    fn words_flags_and_values_are_told_apart() {
        let a = parse(
            &["disk:3", "--label", "My Drive", "--yes", "--size=8G", "extra"],
            &["label"],
        );
        assert_eq!(a.positional, ["disk:3", "extra"]);
        assert_eq!(a.get("label"), Some("My Drive"));
        assert_eq!(a.get("size"), Some("8G"));
        assert!(a.has("yes"));
        assert_eq!(a.get("yes"), None);
    }

    #[test]
    fn short_flags_can_be_bunched() {
        let a = parse(&["-lr", "x"], &[]);
        assert!(a.has("long") && a.has("recursive"));
        assert_eq!(a.positional, ["x"]);
    }

    #[test]
    fn a_dash_number_is_a_word_and_double_dash_ends_options() {
        let a = parse(&["-5", "--", "--not-an-option"], &[]);
        assert_eq!(a.positional, ["-5", "--not-an-option"]);
    }

    #[test]
    fn unknown_options_are_named() {
        let a = parse(&["--bogus"], &[]);
        let err = a.reject_unknown(&["fix"]).unwrap_err();
        assert_eq!(err.to_string(), "unknown option --bogus for this command");
    }

    #[test]
    fn a_missing_value_is_an_error() {
        let items = vec!["--label".to_string()];
        assert!(Args::parse(&items, &["label"]).is_err());
    }
}
