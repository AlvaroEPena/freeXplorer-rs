use fx_core::fatx::{Flavor, Severity};
use fx_core::size;
use fx_core::xbox360::scanner::{self, DetectedPartition};

use crate::args::Args;
use crate::target::{Session, Target, describe, split_partition};
use crate::{Context, Outcome, say};

pub fn list(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["scan"])?;

    let disks = fx_platform::list_disks();
    if disks.is_empty() {
        say!(ctx, "No physical disks found.");
        return Ok(0);
    }

    let elevated = fx_platform::is_elevated();
    for d in &disks {
        let note = if d.is_system {
            "  [system disk]"
        } else if d.has_volumes {
            "  [has file systems mounted]"
        } else {
            ""
        };
        say!(ctx, "disk:{:<3} {}{note}", d.id, d.display());

        if a.has("scan") && elevated && !d.is_system {
            match fx_platform::open_disk(d, false)
                .map_err(Into::into)
                .and_then(|dev| scanner::scan(dev.as_ref()))
            {
                Ok(scan) => say!(ctx, "        {}", describe(scan.kind)),
                Err(err) => say!(ctx, "        couldn't read it: {err}"),
            }
        }
    }
    if a.has("scan") && !elevated {
        say!(
            ctx,
            "\nRun this from an administrator prompt (or with sudo) to look inside the disks."
        );
    } else if !a.has("scan") {
        say!(
            ctx,
            "\nUse \"info disk:N\" to see what's on one, from an administrator prompt (or with sudo)."
        );
    }
    Ok(0)
}

pub fn info(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["usage"])?;
    let spec = a.require(0, "target (disk:N or an image file)")?;

    let target = Target::open(spec, false, false)?;
    let scan = scanner::scan(target.device.as_ref())?;

    say!(ctx, "{}", target.description);
    say!(
        ctx,
        "  {}, {} visible to the console{}",
        describe(scan.kind),
        size::format(scan.visible_length),
        if scan.has_security_sector {
            ", security sector present"
        } else {
            ""
        }
    );
    say!(ctx);

    if scan.partitions.is_empty() {
        say!(
            ctx,
            "  Nothing recognisable here. If this is a blank drive, \"format\" can lay out the Xbox 360 partitions."
        );
        return Ok(0);
    }

    say!(
        ctx,
        "  {:<10} {:<22} {:<14} {:<10} state",
        "id",
        "name",
        "offset",
        "size"
    );
    for p in &scan.partitions {
        let state = state_of(&target, p, a.has("usage"));
        say!(
            ctx,
            "  {:<10} {:<22} {:<14} {:<10} {}",
            p.id(),
            p.name(),
            format!("0x{:X}", p.offset()),
            size::format(p.length()),
            state
        );
    }
    Ok(0)
}

fn state_of(target: &Target, p: &DetectedPartition, with_usage: bool) -> String {
    if !p.spec.is_fatx() {
        return "console cache, not FATX".into();
    }
    if !p.can_mount() {
        return "empty or not formatted".into();
    }
    let described = (|| -> fx_core::Result<String> {
        let mut vol = scanner::open_partition(&target.device, p, false)?;
        let kind = if vol.flavor() == Flavor::Xbox360 {
            "XTAF"
        } else {
            "FATX"
        };
        let mut text = format!(
            "{kind}, {} KB clusters, FAT{}",
            vol.cluster_size() / 1024,
            vol.geometry().fat_entry_size * 8
        );
        if let Some(label) = vol.label()?.filter(|l| !l.is_empty()) {
            text += &format!(", \"{label}\"");
        }
        if with_usage {
            vol.scan_free_space(|_| {})?;
            text += &format!(
                ", {} used of {}",
                size::format(vol.total_bytes() - vol.free_bytes()),
                size::format(vol.total_bytes())
            );
        }
        Ok(text)
    })();
    described.unwrap_or_else(|err| format!("looks damaged ({err})"))
}

pub fn fsck(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["fix", "allow-pc-disk"])?;
    let spec = a.require(0, "target")?;
    let (partition, _) = split_partition(a.positional.get(1..).unwrap_or_default());
    let fix = a.has("fix");

    let mut session = Session::open(spec, partition.as_deref(), fix, a.has("allow-pc-disk"))?;
    say!(
        ctx,
        "checking {} ({})...",
        session.partition.name(),
        size::format(session.partition.length())
    );
    let report = session.volume.check(fix, |_| {})?;
    session.volume.flush()?;

    for issue in &report.issues {
        let severity = match issue.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        say!(ctx, "  {severity:<7} {}", issue.message);
    }
    say!(
        ctx,
        "{} folders, {} files, {} of data, {} clusters in use",
        report.directories,
        report.files,
        size::format(report.file_bytes),
        report.used_clusters
    );
    if report.fixed_clusters > 0 {
        say!(ctx, "released {} leaked clusters", report.fixed_clusters);
    }

    if report.errors() > 0 {
        say!(
            ctx,
            "The volume has real damage. Copy your files off before doing anything else to it."
        );
        return Ok(1);
    }
    if report.warnings() > 0 && !fix {
        say!(
            ctx,
            "No damage, but there's some leftover space that --fix can release."
        );
    } else {
        say!(ctx, "Clean.");
    }
    Ok(0)
}
