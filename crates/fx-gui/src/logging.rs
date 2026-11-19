//! A plain text record of what the window did, next to its settings.
//!
//! The window already keeps the last few hundred lines in memory for its panel. This puts the
//! same lines somewhere that outlives the window, so a bug report can quote them and a crash
//! that takes the window down with it still leaves a message behind.

use std::io::Write;

/// Past this the file starts again, so it can't grow without limit.
const LIMIT: u64 = 2 << 20;

fn path() -> Option<std::path::PathBuf> {
    crate::settings::folder().map(|folder| folder.join("freexplorer-rs.log"))
}

/// Adds one line, stamped so the sequence of a session is obvious.
pub fn line(text: &str) {
    let Some(path) = path() else { return };
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LIMIT) {
        let _ = std::fs::remove_file(&path);
    }
    // not being able to log is never worth failing over: the window has to keep working
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let _ = writeln!(file, "{stamp}  {text}");
}

/// Notes how this run was started, which is the first thing a report needs.
pub fn started() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    line(&format!(
        "---- freeXplorer-rs {} started, elevated={}, args={args:?}",
        env!("CARGO_PKG_VERSION"),
        fx_platform::is_elevated()
    ));
}
