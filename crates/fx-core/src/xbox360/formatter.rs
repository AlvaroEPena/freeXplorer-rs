use super::layout::{self, PartitionSpec};
use crate::device::Device;
use crate::error::{Error, Result};
use crate::fatx::{self, Flavor, FormatOptions, Geometry};

#[derive(Debug, Clone)]
pub struct HddFormatOptions {
    /// 0 picks a cluster size from the partition size (16K for anything under 4 TB)
    pub content_sectors_per_cluster: u32,
    pub content_label: Option<String>,
    /// Content can be made shorter than the rest of the drive, 0 means all of it
    pub content_length: u64,
    /// the 360 itself seems to format the compatibility partition like the others,
    /// but the original Xbox style is available in case it turns out to matter
    pub compat_flavor: Flavor,
}

impl Default for HddFormatOptions {
    fn default() -> Self {
        HddFormatOptions {
            content_sectors_per_cluster: 0,
            content_label: None,
            content_length: 0,
            compat_flavor: Flavor::Xbox360,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FormatStep {
    pub partition: PartitionSpec,
    pub sectors_per_cluster: u32,
    pub geometry: Geometry,
}

/// Works out what would be written without writing anything.
pub fn plan<S: AsRef<str>>(
    device: &Device,
    partition_ids: &[S],
    options: &HddFormatOptions,
) -> Result<Vec<FormatStep>> {
    let visible = layout::visible_length(device.as_ref())?;
    let mut steps = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for raw in partition_ids {
        let id = layout::normalize(raw.as_ref());
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());

        let mut spec = layout::find(&id, visible)
            .ok_or_else(|| Error::invalid(format!("there's no partition called \"{id}\"")))?;
        if !spec.is_fatx() {
            return Err(Error::invalid(format!(
                "{} isn't a FATX partition, there's nothing to format there",
                spec.name
            )));
        }

        let is_content = spec.id == layout::CONTENT_ID;
        if is_content && options.content_length > 0 {
            spec.length = spec.length.min(options.content_length / 512 * 512);
        }
        if spec.length < 0x100_0000 {
            return Err(Error::invalid(format!(
                "the drive is too small, {} would only get {} bytes",
                spec.name, spec.length
            )));
        }
        if spec.end() > device.len() {
            return Err(Error::invalid(format!(
                "{} would end at 0x{:X} but the drive is only 0x{:X} bytes",
                spec.name,
                spec.end(),
                device.len()
            )));
        }

        let spc = match (is_content, options.content_sectors_per_cluster) {
            (true, 0) => Geometry::pick_sectors_per_cluster(spec.length),
            (true, chosen) => chosen,
            (false, _) => 0x20,
        };
        let geometry = Geometry::compute(spec.length, spc)?;
        steps.push(FormatStep {
            partition: spec,
            sectors_per_cluster: spc,
            geometry,
        });
    }
    Ok(steps)
}

pub fn format<S: AsRef<str>>(
    device: &Device,
    partition_ids: &[S],
    options: &HddFormatOptions,
    mut progress: impl FnMut(&FormatStep, f64),
) -> Result<()> {
    for step in plan(device, partition_ids, options)? {
        // never touch the first 512 KB of the drive, that's where the security sector lives
        if step.partition.offset < layout::SYSTEM_CACHE_OFFSET {
            return Err(Error::invalid("refusing to write into the drive header area"));
        }

        let flavor = if step.partition.id == layout::COMPAT_ID {
            options.compat_flavor
        } else {
            Flavor::Xbox360
        };
        let label = if step.partition.id == layout::CONTENT_ID {
            options.content_label.clone()
        } else {
            None
        };
        let format_options = FormatOptions {
            flavor,
            sectors_per_cluster: step.sectors_per_cluster,
            volume_id: None,
            label,
        };
        fatx::format(
            device,
            step.partition.offset,
            step.partition.length,
            &format_options,
            |p| progress(&step, p),
        )?;
        progress(&step, 1.0);
    }
    Ok(())
}
