use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fx_core::xbox360::scanner::{self, DetectedPartition};

use crate::args::Args;
use crate::target::{Target, pick};
use crate::{CliError, Context, Outcome, say, say_err};

// mount <target> [partition|all] [--rw] [--letter X: | --folder PATH] [--hold SECONDS]
pub fn mount(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &["letter", "folder", "hold"])?;
    a.reject_unknown(&["rw", "letter", "folder", "hold", "allow-pc-disk"])?;
    let spec = a.require(0, "target")?;
    if a.has("letter") && !cfg!(windows) {
        return Err(CliError::Usage(
            "--letter is for Windows, give a folder with --folder PATH instead".into(),
        ));
    }
    let which = a.positional.get(1).map(String::as_str);
    let write = a.has("rw");

    if !fx_mount::driver_installed() {
        say_err!(
            ctx,
            "freexplorer-rs: {} isn't installed. It's the free driver that lets this computer show a FATX partition as a drive.",
            fx_mount::DRIVER_NAME
        );
        say_err!(
            ctx,
            "Install it from {} (or: winget install WinFsp.WinFsp on Windows) and run this again.",
            fx_mount::DRIVER_PAGE
        );
        return Ok(1);
    }

    let target = Target::open(spec, write, a.has("allow-pc-disk"))?;
    let scan = scanner::scan(target.device.as_ref())?;

    let chosen: Vec<DetectedPartition> = match which {
        None => all_mountable(&scan)?,
        Some(w) if w.eq_ignore_ascii_case("all") => all_mountable(&scan)?,
        Some(w) => vec![pick(&scan, Some(w))?],
    };
    if (a.has("letter") || a.has("folder")) && chosen.len() > 1 {
        return Err(CliError::Usage(
            "--letter and --folder only make sense for one partition, name it instead of using \"all\"".into(),
        ));
    }
    if a.has("letter") && a.has("folder") {
        return Err(CliError::Usage("pick either --letter or --folder".into()));
    }
    let hold = a.get("hold").map(parse_hold).transpose()?;
    let point = a
        .get("folder")
        .map(str::to_string)
        .or(a.get("letter").map(normalize_letter).transpose()?);

    let mut mounted = Vec::new();
    let result = (|| -> Result<(), CliError> {
        for p in &chosen {
            let mut volume = scanner::open_partition(&target.device, p, write)?;
            say!(ctx, "reading {}...", p.name());
            volume.scan_free_space(|_| {})?;
            let label = volume
                .label()?
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| p.name().to_string());
            let m = fx_mount::mount(volume, &label, point.as_deref()).map_err(|e| CliError::Failed(e.to_string()))?;
            say!(
                ctx,
                "{} is now {} ({})",
                p.name(),
                m.mount_point(),
                if m.read_only() { "read-only" } else { "read/write" }
            );
            mounted.push(m);
        }

        match hold {
            Some(seconds) => {
                say!(ctx, "Mounted for {seconds} seconds (Ctrl+C to stop sooner).");
                let until = Instant::now() + Duration::from_secs(seconds);
                while Instant::now() < until && !ctx.cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            None => {
                say!(ctx, "Press Enter (or Ctrl+C) to unmount.");
                wait_for_enter(ctx);
            }
        }
        Ok(())
    })();

    let mut failed = false;
    for m in mounted {
        say!(ctx, "unmounting {}", m.mount_point());
        if let Err(err) = m.unmount() {
            say_err!(ctx, "freexplorer-rs: {err}");
            failed = true;
        }
    }
    result?;
    Ok(i32::from(failed))
}

fn all_mountable(scan: &scanner::DriveScan) -> Result<Vec<DetectedPartition>, CliError> {
    let found: Vec<DetectedPartition> = scan.partitions.iter().filter(|p| p.can_mount()).cloned().collect();
    if found.is_empty() {
        return Err(CliError::Usage("there are no FATX partitions here to mount".into()));
    }
    Ok(found)
}

fn parse_hold(text: &str) -> Result<u64, CliError> {
    text.parse::<u64>()
        .ok()
        .filter(|&s| s >= 1)
        .ok_or_else(|| CliError::Usage("--hold takes a number of seconds".into()))
}

fn normalize_letter(text: &str) -> Result<String, CliError> {
    let t = text.trim();
    let mut chars = t.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(c), None, _) | (Some(c), Some(':'), None) if c.is_ascii_alphabetic() => {
            Ok(format!("{}:", c.to_ascii_uppercase()))
        }
        _ => Err(CliError::Usage("--letter should look like X:".into())),
    }
}

/// Waits for a line on the real stdin or for Ctrl+C, whichever comes first. The reading thread is
/// left behind if Ctrl+C wins, there is no way to interrupt a read, and the process ends right after.
fn wait_for_enter(ctx: &Context) {
    let pressed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&pressed);
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        flag.store(true, Ordering::Relaxed);
    });
    while !pressed.load(Ordering::Relaxed) && !ctx.cancel.is_cancelled() {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_are_tidied() {
        assert_eq!(normalize_letter("x").unwrap(), "X:");
        assert_eq!(normalize_letter(" q: ").unwrap(), "Q:");
        assert!(normalize_letter("xy").is_err());
        assert!(normalize_letter("1:").is_err());
        assert!(normalize_letter("").is_err());
    }

    #[test]
    fn hold_needs_a_positive_number() {
        assert_eq!(parse_hold("30").unwrap(), 30);
        assert!(parse_hold("0").is_err());
        assert!(parse_hold("soon").is_err());
    }
}
