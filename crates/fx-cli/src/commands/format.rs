use std::fs;
use std::sync::Arc;

use fx_core::Error;
use fx_core::device::{Device, FileDevice};
use fx_core::fatx::{self, Flavor, FormatOptions, Geometry};
use fx_core::size;
use fx_core::xbox360::formatter::{self, HddFormatOptions};
use fx_core::xbox360::layout;
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
    ])?;
    let spec = a.require(0, "target")?;

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
