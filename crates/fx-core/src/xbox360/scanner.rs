use std::sync::Arc;

use super::layout::{self, PartitionFileSystem, PartitionSpec};
use crate::device::{BlockDevice, Device, SliceDevice};
use crate::error::{Error, ErrorKind, Result};
use crate::fatx::{FatxVolume, Flavor, geometry::HEADER_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    /// doesn't look like anything known
    Unknown,
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

    let visible = layout::visible_length(device)?;
    let security = layout::looks_like_security_sector(device)?;
    let mut found = Vec::new();
    let mut any_fatx = false;

    for spec in layout::partitions(visible) {
        if spec.length == 0 || spec.end() > device.len() {
            continue;
        }
        if !spec.is_fatx() {
            found.push(DetectedPartition {
                spec,
                has_signature: false,
                flavor: None,
            });
            continue;
        }
        let flavor = signature_at(device, spec.offset)?;
        any_fatx |= flavor.is_some();
        found.push(DetectedPartition {
            spec,
            has_signature: flavor.is_some(),
            flavor,
        });
    }

    Ok(DriveScan {
        kind: if any_fatx {
            DriveKind::Xbox360Drive
        } else {
            DriveKind::Unknown
        },
        visible_length: visible,
        has_security_sector: security,
        partitions: found,
    })
}

/// Opens a detected partition as a volume.
///
/// Content runs to the end of the drive, and where its FAT ends depends on exactly how long
/// it is. The console goes by the security sector's sector count, other tools might use the
/// whole disk. Guess wrong and every cluster is read from the wrong place, so when the first
/// guess doesn't produce a believable root directory the other one is tried.
pub fn open_partition(device: &Device, partition: &DetectedPartition, writable: bool) -> Result<FatxVolume> {
    if !partition.can_mount() {
        return Err(Error::fatx(
            ErrorKind::NotFatx,
            format!("{} has no FATX signature", partition.name()),
        ));
    }

    let alternative = (device.len() - partition.offset()) / 512 * 512;
    let can_retry = partition.id() == layout::CONTENT_ID && alternative != partition.length();
    let open = |length: u64| -> Result<FatxVolume> {
        let slice: Device = Arc::new(SliceDevice::new(device.clone(), partition.offset(), length, writable)?);
        FatxVolume::open(slice, writable)
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

fn signature_at(device: &dyn BlockDevice, offset: u64) -> Result<Option<Flavor>> {
    if device.len() < 4 || offset > device.len() - 4 {
        return Ok(None);
    }
    let mut sig = [0u8; 4];
    device.read_at(offset, &mut sig)?;
    Ok(Flavor::from_signature(&sig))
}
