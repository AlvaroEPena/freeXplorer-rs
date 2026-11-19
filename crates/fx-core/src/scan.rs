//! Working out what a drive is before looking inside it.
//!
//! The order matters. A single FATX partition has to be checked first, because a dump of one
//! partition starts with the same bytes a whole drive's volume would. Then the original Xbox,
//! whose table and partitions live at offsets no 360 layout uses. Then the 360, which knows its
//! layout whether or not anything has been written to the drive.

use crate::device::{BlockDevice, Device};
use crate::error::Result;
use crate::fatx::{Flavor, geometry::HEADER_SIZE};
use crate::og;
use crate::partition::{PartitionFileSystem, PartitionSpec};
use crate::xbox360;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    /// doesn't look like anything known
    Unknown,
    /// an original Xbox drive: a table or retail FATX volumes, and no security sector
    OgDrive,
    /// a whole 360 drive, at least one FATX partition is where the console would put it
    Xbox360Drive,
    /// a dump of one FATX partition, signature right at the start
    SinglePartition,
}

#[derive(Debug, Clone)]
pub struct DetectedPartition {
    pub spec: PartitionSpec,
    pub has_signature: bool,
    pub flavor: Option<Flavor>,
}

impl DetectedPartition {
    pub fn id(&self) -> &str {
        self.spec.id
    }

    pub fn name(&self) -> &str {
        self.spec.name
    }

    pub fn offset(&self) -> u64 {
        self.spec.offset
    }

    pub fn length(&self) -> u64 {
        self.spec.length
    }

    pub fn can_mount(&self) -> bool {
        self.spec.is_fatx() && self.has_signature
    }
}

#[derive(Debug, Clone)]
pub struct DriveScan {
    pub kind: DriveKind,
    pub visible_length: u64,
    pub has_security_sector: bool,
    pub partitions: Vec<DetectedPartition>,
}

pub const SINGLE_PARTITION_ID: &str = "image";

pub fn scan(device: &dyn BlockDevice) -> Result<DriveScan> {
    let mut scan = detect(device)?;
    sort_for_reading(&mut scan);
    Ok(scan)
}

/// Put the partitions in the order somebody reading the list expects: by name, so an original
/// Xbox drive reads C, E, F, G, X, Y, Z rather than the order the table happens to store them in,
/// which starts with the cache drives the console writes first. Sorting here rather than in each
/// list keeps the command line, the window and the dialogs saying the same thing.
fn sort_for_reading(scan: &mut DriveScan) {
    scan.partitions.sort_by(|a, b| a.id().cmp(b.id()));
}

fn detect(device: &dyn BlockDevice) -> Result<DriveScan> {
    if device.len() >= HEADER_SIZE
        && let Some(flavor) = signature_at(device, 0)?
    {
        let spec = PartitionSpec {
            id: SINGLE_PARTITION_ID,
            name: "Partition image",
            offset: 0,
            length: device.len() / 512 * 512,
            file_system: PartitionFileSystem::Fatx,
            description: "A single FATX partition.",
        };
        return Ok(DriveScan {
            kind: DriveKind::SinglePartition,
            visible_length: spec.length,
            has_security_sector: false,
            partitions: vec![DetectedPartition {
                spec,
                has_signature: true,
                flavor: Some(flavor),
            }],
        });
    }

    if let Some(scan) = og::scanner::scan(device)? {
        return Ok(scan);
    }

    xbox360::scanner::scan_drive(device)
}

pub fn signature_at(device: &dyn BlockDevice, offset: u64) -> Result<Option<Flavor>> {
    if device.len() < 4 || offset > device.len() - 4 {
        return Ok(None);
    }
    let mut sig = [0u8; 4];
    device.read_at(offset, &mut sig)?;
    Ok(Flavor::from_signature(&sig))
}

/// Opens a detected partition as a volume.
///
/// The 360's content partition runs to the end of the drive, and where its FAT ends depends on
/// exactly how long it is: the console goes by the security sector's sector count, other tools
/// use the whole disk. Guess wrong and every cluster is read from the wrong place, so when the
/// first guess doesn't produce a believable root directory the other one is tried. Every other
/// partition has one honest length, so nothing else needs the second attempt.
pub fn open_partition(
    device: &Device,
    partition: &DetectedPartition,
    writable: bool,
) -> Result<crate::fatx::FatxVolume> {
    use std::sync::Arc;

    use crate::device::SliceDevice;
    use crate::error::{Error, ErrorKind};

    if !partition.can_mount() {
        return Err(Error::fatx(
            ErrorKind::NotFatx,
            format!("{} has no FATX signature", partition.name()),
        ));
    }

    let alternative = (device.len() - partition.offset()) / 512 * 512;
    let can_retry = partition.id() == xbox360::layout::CONTENT_ID && alternative != partition.length();
    let open = |length: u64| -> Result<crate::fatx::FatxVolume> {
        let slice: Device = Arc::new(SliceDevice::new(device.clone(), partition.offset(), length, writable)?);
        crate::fatx::FatxVolume::open(slice, writable)
    };

    let mut first = None;
    match open(partition.length()) {
        Ok(mut volume) => {
            if !can_retry || volume.root_looks_sane() {
                return Ok(volume);
            }
            first = Some(volume);
        }
        Err(err) if can_retry && err.kind().is_some() => {}
        Err(err) => return Err(err),
    }

    match open(alternative) {
        Ok(mut second) => {
            if second.root_looks_sane() || first.is_none() {
                return Ok(second);
            }
        }
        Err(err) if first.is_some() && err.kind().is_some() => {}
        Err(err) => return Err(err),
    }
    Ok(first.expect("the first attempt worked if it's needed here"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemoryDevice;

    #[test]
    fn a_single_partition_image_is_recognised_by_its_signature_at_the_start() {
        let device = MemoryDevice::new(0x4000_0000);
        device.write_at(0, &[0u8; 16]).unwrap();
        device.write_at(0, b"FATX").unwrap();
        assert!(signature_at(&device, 0).unwrap().is_some());
    }

    #[test]
    fn a_signature_past_the_end_of_the_device_is_not_read() {
        let device = MemoryDevice::new(4);
        assert!(signature_at(&device, 0x1000).unwrap().is_none());
    }

    fn spec(id: &'static str) -> DetectedPartition {
        DetectedPartition {
            spec: PartitionSpec {
                id,
                name: id,
                offset: 0,
                length: 0,
                file_system: PartitionFileSystem::Fatx,
                description: "",
            },
            has_signature: true,
            flavor: None,
        }
    }

    #[test]
    fn the_list_reads_by_name_not_in_the_order_the_table_keeps() {
        // the table stores an original Xbox drive's partitions starting with the cache drives
        let mut scan = DriveScan {
            kind: DriveKind::OgDrive,
            visible_length: 0,
            has_security_sector: false,
            partitions: ["x", "y", "z", "c", "e", "f"].map(spec).to_vec(),
        };
        sort_for_reading(&mut scan);
        let ids: Vec<&str> = scan.partitions.iter().map(|p| p.id()).collect();
        assert_eq!(ids, ["c", "e", "f", "x", "y", "z"]);
    }

    #[test]
    fn a_360_drive_is_listed_by_name_too() {
        let device: Device = std::sync::Arc::new(MemoryDevice::new(6 << 30));
        crate::xbox360::formatter::format(
            &device,
            &["content", "sysext", "compat", "sysext2"],
            &Default::default(),
            |_, _| {},
        )
        .unwrap();
        let scan = scan(device.as_ref()).unwrap();
        let ids: Vec<&str> = scan.partitions.iter().map(|p| p.id()).collect();
        assert_eq!(
            ids,
            ["compat", "content", "gamecache", "syscache", "sysext", "sysext2"]
        );
    }
}
