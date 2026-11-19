use std::fs;
use std::sync::Arc;

use fx_core::Error;
use fx_core::device::{Device, FileDevice};
use fx_core::fatx::{self, Flavor, FormatOptions, Geometry};
use fx_core::og;
use fx_core::size;
use fx_core::xbox360::formatter::{self, HddFormatOptions};
use fx_core::xbox360::layout;
use fx_core::xbox360::scanner::{self, DriveKind};
use fx_platform::KeepAwake;

use super::{confirm, percent};
use crate::args::Args;
use crate::target::Target;
use crate::{CliError, Context, Outcome, parse_size, say, sizes};

const VALUES: &[&str] = &[
    "partitions",
    "label",
    "cluster-size",
    "content-size",
    "confirm",
    "compat-flavor",
    "size",
    "layout",
    "og-layout",
];

const EVERY_PARTITION: [&str; 4] = [
    layout::SYSTEM_EXT_ID,
    layout::SYSTEM_EXT2_ID,
    layout::COMPAT_ID,
    layout::CONTENT_ID,
];

fn parse_flavor(text: Option<&str>) -> Result<Flavor, CliError> {
    match text.map(str::to_lowercase).as_deref() {
        None | Some("xbox360" | "360" | "xtaf") => Ok(Flavor::Xbox360),
        Some("xbox1" | "xbox" | "fatx") => Ok(Flavor::Xbox),
        _ => Err(CliError::Usage("--compat-flavor is xbox360 or xbox1".into())),
    }
}

/// Which console's layout a format writes. A blank drive has no kind to detect, so the layout is
/// something the person asking has to say.
fn parse_layout(text: Option<&str>) -> Result<Option<bool>, CliError> {
    match text.map(str::to_lowercase).as_deref() {
        None => Ok(None),
        Some("og" | "xbox1" | "xbox" | "fatx") => Ok(Some(true)),
        Some("360" | "xbox360" | "xtaf") => Ok(Some(false)),
        Some(other) => Err(CliError::Usage(format!(
            "--layout is og or 360, not \"{other}\""
        ))),
    }
}

fn parse_og_layout(text: Option<&str>) -> Result<og::Layout, CliError> {
    match text.map(str::to_lowercase).as_deref() {
        None | Some("auto") => Ok(og::Layout::Auto),
        Some("f-all" | "fall") => Ok(og::Layout::FAll),
        Some(other) => Err(CliError::Usage(format!(
            "--og-layout is auto or f-all, not \"{other}\""
        ))),
    }
}

pub fn format(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, VALUES)?;
    a.reject_unknown(&[
        "all",
        "partitions",
        "whole",
        "label",
        "cluster-size",
        "content-size",
        "dry-run",
        "yes",
        "confirm",
        "allow-pc-disk",
        "compat-flavor",
        "layout",
        "og-layout",
    ])?;
    let spec = a.require(0, "target")?;

    if parse_layout(a.get("layout"))? == Some(true) {
        return format_og(&a, spec, ctx);
    }

    let whole = a.has("whole");
    let layout_asked = a.has("all") || a.has("partitions");
    if whole == layout_asked {
        return Err(CliError::Usage(
            "say what to format: --all (the four Xbox 360 partitions), --partitions content,compat,... \
             or --whole (one volume over the entire image)"
                .into(),
        ));
    }
    if whole && fx_platform::parse_disk_spec(spec).is_some() {
        return Err(CliError::Usage(
            "--whole is only for image files. A physical disk keeps the Xbox 360 layout.".into(),
        ));
    }

    let dry_run = a.has("dry-run");
    let target = Target::open(spec, !dry_run, a.has("allow-pc-disk"))?;
    refuse_an_og_drive(&target)?;

    let cluster_sectors = a
        .get("cluster-size")
        .map(sizes::cluster_sectors)
        .transpose()?
        .unwrap_or(0);
    let label = a.get("label").map(str::to_string);
    if label.as_ref().is_some_and(|l| l.chars().count() > 15) {
        return Err(CliError::Usage("labels can be 15 characters at most".into()));
    }

    say!(ctx, "{}", target.description);

    let mut whole_job = None;
    let mut hdd_job = None;
    if whole {
        let length = target.device.len() / 512 * 512;
        let spc = if cluster_sectors != 0 {
            cluster_sectors
        } else {
            Geometry::pick_sectors_per_cluster(length)
        };
        let geometry = Geometry::compute(length, spc)?;
        say!(
            ctx,
            "  whole image  {}, {} KB clusters, FAT{}",
            size::format(length),
            geometry.cluster_size / 1024,
            geometry.fat_entry_size * 8
        );
        whole_job = Some((length, spc));
    } else {
        let ids: Vec<String> = if a.has("all") {
            EVERY_PARTITION.iter().map(|s| s.to_string()).collect()
        } else {
            a.get("partitions")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        let options = HddFormatOptions {
            content_sectors_per_cluster: cluster_sectors,
            content_label: label.clone(),
            content_length: a.get("content-size").map(parse_size).transpose()?.unwrap_or(0),
            compat_flavor: parse_flavor(a.get("compat-flavor"))?,
        };
        let plan = formatter::plan(&target.device, &ids, &options).map_err(|err| match err {
            Error::Invalid(message) => CliError::Usage(message),
            other => other.into(),
        })?;
        for step in &plan {
            say!(
                ctx,
                "  {:<22} 0x{:X} {:>9}, {} KB clusters, FAT{}",
                step.partition.name,
                step.partition.offset,
                size::format(step.partition.length),
                step.geometry.cluster_size / 1024,
                step.geometry.fat_entry_size * 8
            );
        }
        hdd_job = Some((ids, options));
    }

    if dry_run {
        say!(ctx, "Dry run, nothing was written.");
        return Ok(0);
    }

    say!(ctx, "Everything on the partitions listed above will be erased.");
    if !confirm(ctx, &target, a.get("confirm"), a.has("yes")) {
        say!(ctx, "Not confirmed, nothing was changed.");
        return Ok(1);
    }

    let _awake = KeepAwake::new("Formatting a drive");
    let mut last_shown = -1.0f64;
    let mut show = |ctx: &mut Context, p: f64| {
        if p - last_shown >= 0.25 || p >= 1.0 {
            last_shown = if p >= 1.0 { -1.0 } else { p };
            say!(ctx, "  {}", percent(p));
        }
    };

    if let Some((length, spc)) = whole_job {
        let options = FormatOptions {
            sectors_per_cluster: spc,
            label,
            ..FormatOptions::default()
        };
        fatx::format(&target.device, 0, length, &options, |p| show(ctx, p))?;
    }
    if let Some((ids, options)) = hdd_job {
        formatter::format(&target.device, &ids, &options, |_, p| show(ctx, p))?;
    }
    target.device.flush()?;
    say!(ctx, "Formatted.");
    Ok(0)
}

/// The one thing the 360 formatter must never be pointed at: an original Xbox drive.
///
/// The 360 layout is compiled in and its formatter writes XTAF volumes at 360 offsets without ever
/// touching sector zero. On an OG drive that leaves the table in place and puts a system cache
/// across X, Y and part of Z, so the drive comes back as a hybrid that is neither console's.
fn refuse_an_og_drive(target: &Target) -> Result<(), CliError> {
    if scanner::scan(target.device.as_ref())?.kind == DriveKind::OgDrive {
        return Err(CliError::Usage(
            "this is an original Xbox drive, whose partitions are the drive's own. Say --layout og: \
             --all lays the drive out again, --partitions f reformats one in place."
                .into(),
        ));
    }
    Ok(())
}

fn usage(err: Error) -> CliError {
    match err {
        Error::Invalid(message) => CliError::Usage(message),
        other => other.into(),
    }
}

/// `format --layout og`: the drive's own layout, read from its table or planned from its length.
fn format_og(a: &Args, spec: &str, ctx: &mut Context) -> Outcome {
    let dry_run = a.has("dry-run");
    let target = Target::open(spec, !dry_run, a.has("allow-pc-disk"))?;
    say!(ctx, "{}", target.description);

    let cluster_sectors = a
        .get("cluster-size")
        .map(sizes::cluster_sectors)
        .transpose()?
        .unwrap_or(0);
    let label = a.get("label").map(str::to_string);
    if label.as_ref().is_some_and(|l| l.chars().count() > 15) {
        return Err(CliError::Usage("labels can be 15 characters at most".into()));
    }
    let options = og::Options {
        layout: parse_og_layout(a.get("og-layout"))?,
        sectors_per_cluster: cluster_sectors,
        label,
    };

    let all = a.has("all");
    let ids: Vec<String> = a
        .get("partitions")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if all && !ids.is_empty() {
        return Err(CliError::Usage(
            "--all lays the whole drive out and --partitions reformats what's there, so say one \
             or the other"
                .into(),
        ));
    }
    if !all && ids.is_empty() {
        return Err(CliError::Usage(
            "say what to format: --all lays the whole drive out, table and every partition, \
             --partitions f reformats partitions the drive already has"
                .into(),
        ));
    }
    if !all && a.has("og-layout") {
        return Err(CliError::Usage(
            "--og-layout decides how the space past E is divided, so it only goes with --all".into(),
        ));
    }

    let steps = if all {
        let steps = og::steps(target.device.len(), &options).map_err(usage)?;
        say!(
            ctx,
            "  laying out {} partitions, and rewriting the table in sector 0",
            steps.len()
        );
        say!(
            ctx,
            "  the boot loader from 0x200 to 0x10000 is left exactly as it is"
        );
        steps
    } else {
        let table = og::mbr::read(target.device.as_ref())?.ok_or_else(|| {
            CliError::Usage(
                "this drive has no original Xbox partition table, so there are no partitions to \
                 reformat. Use --all to lay it out."
                    .into(),
            )
        })?;
        let specs = og::from_table(&table, target.device.len());
        let mut chosen = Vec::new();
        for id in &ids {
            let found = og::find(&specs, id).ok_or_else(|| {
                CliError::Usage(format!(
                    "this drive's table has no partition \"{id}\". It has: {}",
                    specs.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
                ))
            })?;
            if found.length == 0 {
                return Err(CliError::Usage(format!(
                    "{} has no space on this drive to format",
                    found.name
                )));
            }
            chosen.push(found.clone());
        }
        og::steps_for(&chosen, &options).map_err(usage)?
    };

    for step in &steps {
        say!(
            ctx,
            "  {:<22} LBA 0x{:X} {:>9}, {} KB clusters, FAT{}, {} clusters",
            step.spec.name,
            step.spec.offset / 512,
            size::format(step.spec.length),
            step.geometry.cluster_size / 1024,
            step.geometry.fat_entry_size * 8,
            step.geometry.cluster_count
        );
    }

    if dry_run {
        say!(ctx, "Dry run, nothing was written.");
        return Ok(0);
    }

    say!(ctx, "Everything in the partitions listed above will be erased.");
    if !confirm(ctx, &target, a.get("confirm"), a.has("yes")) {
        say!(ctx, "Not confirmed, nothing was changed.");
        return Ok(1);
    }

    let _awake = KeepAwake::new("Formatting a drive");
    let mut last_shown = -1.0f64;
    let mut show = |ctx: &mut Context, p: f64| {
        if p - last_shown >= 0.25 || p >= 1.0 {
            last_shown = if p >= 1.0 { -1.0 } else { p };
            say!(ctx, "  {}", percent(p));
        }
    };

    if all {
        og::drive(&target.device, &steps, &options, |_, p| show(ctx, p))?;
    } else {
        og::volumes(&target.device, &steps, &options, |_, p| show(ctx, p))?;
    }
    target.device.flush()?;

    // the house standard: a format isn't finished until what it wrote reads back clean
    say!(ctx, "Reading it back...");
    og::verify(&target.device, &steps, &options)?;
    say!(ctx, "Formatted. Every partition reads back clean.");
    Ok(0)
}

pub fn new_image(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, VALUES)?;
    a.reject_unknown(&["size", "partition", "label", "cluster-size"])?;
    let path = a.require(0, "file name for the new image")?;
    let Some(size_text) = a.get("size") else {
        return Err(CliError::Usage("how big? for example --size 8G".into()));
    };
    if std::path::Path::new(path).exists() {
        return Err(CliError::Usage(format!("{path} already exists, not overwriting it")));
    }

    let length = parse_size(size_text)? / 512 * 512;
    let single = a.has("partition");
    if !single && length < layout::MINIMUM_DRIVE_LENGTH {
        return Err(CliError::Usage(format!(
            "a full Xbox 360 layout needs at least {}. Make it bigger, or add --partition for a single volume.",
            size::format(layout::MINIMUM_DRIVE_LENGTH)
        )));
    }

    let cluster_sectors = a
        .get("cluster-size")
        .map(sizes::cluster_sectors)
        .transpose()?
        .unwrap_or(0);
    let label = a.get("label").map(str::to_string);

    let image = FileDevice::create_image(path, length, true)?;
    let name = fx_core::BlockDevice::name(&image).to_string();
    let device: Device = Arc::new(image);
    let made = (|| -> Result<(), Error> {
        if single {
            let options = FormatOptions {
                sectors_per_cluster: cluster_sectors,
                label: label.clone(),
                ..FormatOptions::default()
            };
            fatx::format(&device, 0, length, &options, |_| {})?;
        } else {
            let options = HddFormatOptions {
                content_sectors_per_cluster: cluster_sectors,
                content_label: label.clone(),
                ..HddFormatOptions::default()
            };
            formatter::format(&device, &EVERY_PARTITION, &options, |_, _| {})?;
        }
        device.flush()?;
        Ok(())
    })();
    drop(device);
    if let Err(err) = made {
        let _ = fs::remove_file(path);
        return Err(err.into());
    }
    say!(
        ctx,
        "created {name} ({}{})",
        size::format(length),
        if single {
            ", one FATX volume"
        } else {
            ", Xbox 360 layout"
        }
    );
    Ok(0)
}
