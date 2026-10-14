use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fx_core::Error;
use fx_core::backup::{self, BackupOptions, RestoreOptions};
use fx_core::device::FileDevice;
use fx_core::size;
use fx_core::xbox360::security;
use fx_platform::KeepAwake;

use super::{ConsoleProgress, confirm};
use crate::args::Args;
use crate::target::Target;
use crate::{CliError, Context, Outcome, say};

// backup <disk:N | image> <file> [--no-verify] [--no-sparse]
pub fn backup(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["no-verify", "no-sparse"])?;
    let spec = a.require(0, "drive to back up (disk:N or an image file)")?;
    let file = Path::new(a.require(1, "file to save the backup in")?);

    let source = Target::open(spec, false, false)?;
    let length = source.device.len();

    let check = backup::check_destination(file, length, fx_platform::destination_info);
    if let Some(problem) = check.problem {
        return Err(CliError::Usage(problem));
    }

    say!(ctx, "{}, {}", source.description, size::format(length));
    say!(ctx, "saving to {}", std::path::absolute(file)?.display());
    if let Some(warning) = check.warning {
        say!(ctx, "{warning}");
    }

    let options = BackupOptions {
        verify: !a.has("no-verify"),
        sparse: !a.has("no-sparse"),
    };
    let _awake = KeepAwake::new("Backing up a drive");
    let cancel = ctx.cancel.clone();
    let outcome = backup::create(
        source.device.as_ref(),
        file,
        &options,
        &mut ConsoleProgress::new(ctx),
        &cancel,
    );
    match outcome {
        Ok(result) => {
            say!(
                ctx,
                "Done. {} saved, taking {} on disk{}.",
                size::format(result.length),
                size::format(result.size_on_disk),
                if result.verified { ", read back and checked" } else { "" }
            );
            say!(ctx, "SHA-256 {}", result.sha256);
            Ok(0)
        }
        Err(Error::Cancelled) => {
            say!(ctx, "Stopped, the partly written backup was deleted.");
            Ok(1)
        }
        Err(other) => Err(other.into()),
    }
}

// restore <file> <disk:N | image> [--keep-header] [--no-verify] [--confirm "disk 4"] [--yes] [--allow-pc-disk]
pub fn restore(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &["confirm"])?;
    a.reject_unknown(&["no-verify", "yes", "confirm", "allow-pc-disk", "keep-header"])?;
    let file = Path::new(a.require(0, "backup file")?);
    let spec = a.require(1, "drive to restore to (disk:N or an image file)")?;
    if !file.is_file() {
        return Err(CliError::Usage(format!("can't find the backup \"{}\"", file.display())));
    }
    let length = fs::metadata(file)?.len();

    // restoring into an image file that isn't there yet just makes it
    let fresh = fx_platform::parse_disk_spec(spec).is_none() && !Path::new(spec).exists();
    let keep = a.has("keep-header");
    if keep && fresh {
        return Err(CliError::Usage(
            "a new file has no security sector of its own to keep".into(),
        ));
    }
    if fresh {
        FileDevice::create_image(spec, length, true)?;
    }

    let target = match Target::open(spec, true, a.has("allow-pc-disk")) {
        Ok(target) => target,
        Err(err) => {
            if fresh {
                let _ = fs::remove_file(spec);
            }
            return Err(err);
        }
    };

    let started = Arc::new(AtomicBool::new(false));
    let outcome = restore_into(&a, ctx, &target, file, length, fresh, keep, &started);
    drop(target);

    match outcome {
        Err(CliError::Cancelled) => {
            if fresh {
                let _ = fs::remove_file(spec);
            }
            say!(
                ctx,
                "{}",
                if fresh {
                    "Stopped, the partly written image was deleted."
                } else if started.load(Ordering::Relaxed) {
                    "Stopped part way. The drive now holds a partial copy and won't work until it is restored again."
                } else {
                    "Stopped before anything was written."
                }
            );
            Ok(1)
        }
        Err(other) => {
            if fresh {
                let _ = fs::remove_file(spec);
            }
            Err(other)
        }
        Ok(code) => Ok(code),
    }
}

#[allow(clippy::too_many_arguments)]
fn restore_into(
    a: &Args,
    ctx: &mut Context,
    target: &Target,
    file: &Path,
    length: u64,
    fresh: bool,
    keep: bool,
    started: &Arc<AtomicBool>,
) -> Outcome {
    let target_length = target.device.len();
    if length > target_length {
        return Err(CliError::Usage(format!(
            "the backup is {} and {} is only {}. A drive has to be at least as big as the one that was backed up.",
            size::format(length),
            target.description,
            size::format(target_length)
        )));
    }

    if keep {
        let image = FileDevice::open(file, false)?;
        if let Some(problem) = security::keep_problem_for(&image, target.device.as_ref())? {
            return Err(CliError::Usage(problem));
        }
    }

    say!(ctx, "{}, {}", target.description, size::format(target_length));
    let full = std::path::absolute(file)?;
    if !fresh {
        if keep {
            say!(
                ctx,
                "Everything on it except its own first {} (the security sector) will be overwritten with {} ({}).",
                size::format(security::HEADER_LENGTH),
                full.display(),
                size::format(length)
            );
        } else {
            say!(
                ctx,
                "Everything on it will be overwritten with {} ({}).",
                full.display(),
                size::format(length)
            );
        }
        if !confirm(ctx, target, a.get("confirm"), a.has("yes")) {
            say!(ctx, "Not confirmed, nothing was changed.");
            return Ok(1);
        }
    }

    let flag = Arc::clone(started);
    let options = RestoreOptions {
        verify: !a.has("no-verify"),
        skip_first_bytes: if keep { security::HEADER_LENGTH } else { 0 },
        writing_started: Some(Box::new(move || flag.store(true, Ordering::Relaxed))),
    };
    let _awake = KeepAwake::new("Restoring a drive");
    let cancel = ctx.cancel.clone();
    let result = backup::restore(
        file,
        target.device.as_ref(),
        options,
        &mut ConsoleProgress::new(ctx),
        &cancel,
    )?;

    say!(
        ctx,
        "Done. {} written{}.",
        size::format(result.length - result.skipped),
        if result.verified { ", read back and checked" } else { "" }
    );
    if result.skipped > 0 {
        say!(
            ctx,
            "The first {} of the drive, with its own security sector, was left as it was.",
            size::format(result.skipped)
        );
    }
    if !result.checksum_file_used {
        say!(
            ctx,
            "There was no .sha256 file beside the backup, so the backup itself couldn't be checked for damage."
        );
    }
    Ok(0)
}
