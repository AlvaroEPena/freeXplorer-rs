use std::sync::Arc;

use super::volume::FatxVolume;
use super::{
    Flavor,
    geometry::{Geometry, HEADER_SIZE, MAX_DATA_CLUSTERS},
};
use crate::device::{Device, SliceDevice};
use crate::error::{Error, ErrorKind, Result};

#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub flavor: Flavor,
    /// 0 means pick one from the partition size
    pub sectors_per_cluster: u32,
    pub volume_id: Option<u32>,
    pub label: Option<String>,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            flavor: Flavor::Xbox360,
            sectors_per_cluster: 0,
            volume_id: None,
            label: None,
        }
    }
}

/// Writes an empty FATX volume into `[offset, offset + length)` of the device and nothing
/// outside of it. Whatever was in the data area stays there, it's just unreferenced.
pub fn format(
    device: &Device,
    offset: u64,
    length: u64,
    options: &FormatOptions,
    mut progress: impl FnMut(f64),
) -> Result<Geometry> {
    if !device.can_write() {
        return Err(Error::fatx(ErrorKind::ReadOnly, "the device is read-only"));
    }
    if length == 0 || offset > device.len() || length > device.len() - offset {
        return Err(Error::invalid("partition doesn't fit on the device"));
    }

    let spc = if options.sectors_per_cluster != 0 {
        options.sectors_per_cluster
    } else {
        Geometry::pick_sectors_per_cluster(length)
    };
    let geometry = Geometry::compute(length, spc)?;
    if geometry.cluster_count as u64 > MAX_DATA_CLUSTERS {
        return Err(Error::fatx(
            ErrorKind::InvalidOperation,
            format!(
                "{} clusters is more than the console accepts, use a bigger cluster size",
                geometry.cluster_count
            ),
        ));
    }

    let big = options.flavor.is_big_endian();
    let volume_id = options.volume_id.unwrap_or_else(random_id);
    let word = |value: u32| if big { value.to_be_bytes() } else { value.to_le_bytes() };

    // header: signature, id, sectors per cluster, root cluster, two zero bytes, then 0xFF to the end of
    // the page. That's what a console-formatted drive has.
    let mut header = vec![0xFFu8; HEADER_SIZE as usize];
    header[..4].copy_from_slice(options.flavor.signature());
    header[4..8].copy_from_slice(&word(volume_id));
    header[8..12].copy_from_slice(&word(spc));
    header[12..16].copy_from_slice(&word(1));
    header[0x10] = 0;
    header[0x11] = 0;
    device.write_at(offset, &header)?;

    // fat: everything free apart from entry 0 (media) and entry 1 (the root directory's single cluster)
    let fat_start = offset + geometry.fat_offset();
    const CHUNK: u64 = 1 << 20;
    let zeros = vec![0u8; CHUNK.min(geometry.fat_size) as usize];
    let mut written = 0u64;
    while written < geometry.fat_size {
        let n = (zeros.len() as u64).min(geometry.fat_size - written) as usize;
        if written == 0 {
            let mut first = vec![0u8; n];
            set_entry(&mut first, &geometry, 0, geometry.media_entry(), big);
            set_entry(&mut first, &geometry, 1, geometry.end_of_chain(), big);
            device.write_at(fat_start, &first)?;
        } else {
            device.write_at(fat_start + written, &zeros[..n])?;
        }
        written += n as u64;
        progress(written as f64 / geometry.fat_size as f64);
    }

    // empty root directory: a cluster of 0xFF is "no entries"
    let root = vec![0xFFu8; geometry.cluster_size as usize];
    device.write_at(offset + geometry.cluster_offset(1), &root)?;
    device.flush()?;

    if let Some(label) = options.label.as_deref().filter(|l| !l.is_empty()) {
        let slice: Device = Arc::new(SliceDevice::new(device.clone(), offset, length, true)?);
        let mut volume = FatxVolume::open(slice, true)?;
        volume.set_label(label)?;
        volume.close()?;
    }
    Ok(geometry)
}

fn set_entry(fat: &mut [u8], g: &Geometry, index: usize, value: u32, big: bool) {
    let size = g.fat_entry_size as usize;
    let target = &mut fat[index * size..(index + 1) * size];
    match (size, big) {
        (2, true) => target.copy_from_slice(&(value as u16).to_be_bytes()),
        (2, false) => target.copy_from_slice(&(value as u16).to_le_bytes()),
        (_, true) => target.copy_from_slice(&value.to_be_bytes()),
        (_, false) => target.copy_from_slice(&value.to_le_bytes()),
    }
}

fn random_id() -> u32 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        // no random source: the clock is as good as anything for an id nobody reads
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos());
        bytes = nanos.to_le_bytes();
    }
    (u32::from_le_bytes(bytes) % (i32::MAX as u32 - 1)) + 1
}
