//! The one thing the window remembers: light or dark.

use std::path::PathBuf;

fn file() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("freeXplorer-rs").join("settings.json"))
}

pub fn saved_dark() -> Option<bool> {
    let text = std::fs::read_to_string(file()?).ok()?;
    parse(&text)
}

fn parse(text: &str) -> Option<bool> {
    let after = text.split("\"dark\"").nth(1)?;
    let value = after.trim_start().strip_prefix(':')?.trim_start();
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// It just won't be remembered if this can't be written.
pub fn save_dark(dark: bool) {
    if let Some(path) = file() {
        if let Some(folder) = path.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let _ = std::fs::write(path, format!("{{ \"dark\": {dark} }}\n"));
    }
}

/// What the system is set to, for the first start.
pub fn system_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .creation_flags_hidden()
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("0x0"))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    false
}

#[cfg(windows)]
trait HiddenWindow {
    fn creation_flags_hidden(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl HiddenWindow for std::process::Command {
    fn creation_flags_hidden(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_what_it_wrote() {
        assert_eq!(parse("{ \"dark\": true }"), Some(true));
        assert_eq!(parse("{\"dark\":false}"), Some(false));
        assert_eq!(parse("{ \"dark\": 3 }"), None);
        assert_eq!(parse("nothing"), None);
    }
}
