use std::path::Path;
use std::sync::Arc;

use fx_core::device::{Device, FileDevice};
use fx_core::fatx::FatxVolume;
use fx_core::xbox360::layout;
use fx_core::xbox360::scanner::{self, DetectedPartition, DriveKind, DriveScan};
use fx_platform::{DiskInfo, safety};

use crate::CliError;

/// What the user pointed us at: a physical disk or an image file.
pub struct Target {
    pub device: Device,
    pub description: String,
    pub disk: Option<DiskInfo>,
}

impl Target {
    pub fn open(spec: &str, writable: bool, allow_pc_disk: bool) -> Result<Target, CliError> {
        if let Some(id) = fx_platform::parse_disk_spec(spec) {
            let info = fx_platform::list_disks()
                .into_iter()
                .find(|d| d.id == id)
                .ok_or_else(|| {
                    CliError::Failed(format!("there's no disk {id}. Run \"list\" to see what's connected."))
                })?;
            if writable {
                safety::check_write(&info, allow_pc_disk).map_err(CliError::Failed)?;
            }
            let device = fx_platform::open_disk(&info, writable)?;
            return Ok(Target {
                device,
                description: info.display(),
                disk: Some(info),
            });
        }

        if !Path::new(spec).is_file() {
            return Err(CliError::Failed(format!(
                "can't find \"{spec}\". Give a disk like disk:3 or the path of an image file."
            )));
        }
        let device = FileDevice::open(spec, writable)?;
        let description = device_name(&device);
        Ok(Target {
            device: Arc::new(device),
            description,
            disk: None,
        })
    }
}

fn device_name(device: &FileDevice) -> String {
    use fx_core::BlockDevice;
    device.name().to_string()
}

pub fn describe(kind: DriveKind) -> &'static str {
    match kind {
        DriveKind::Xbox360Drive => "Xbox 360 drive",
        DriveKind::SinglePartition => "single FATX partition image",
        DriveKind::Unknown => "no Xbox 360 partitions found",
    }
}

/// The partition the user asked for, or the obvious one if they didn't say.
pub fn pick(scan: &DriveScan, name: Option<&str>) -> Result<DetectedPartition, CliError> {
    let Some(name) = name else {
        let mountable: Vec<&DetectedPartition> = scan.partitions.iter().filter(|p| p.can_mount()).collect();
        return match mountable.as_slice() {
            [only] => Ok((*only).clone()),
            [] => Err(CliError::Usage("there are no FATX partitions here to open".into())),
            many => Err(CliError::Usage(format!(
                "say which partition: {}",
                many.iter().map(|p| p.id()).collect::<Vec<_>>().join(", ")
            ))),
        };
    };

    let id = if name.eq_ignore_ascii_case(scanner::SINGLE_PARTITION_ID) {
        scanner::SINGLE_PARTITION_ID.to_string()
    } else {
        layout::normalize(name)
    };
    let found = scan.partitions.iter().find(|p| p.id() == id).ok_or_else(|| {
        CliError::Usage(format!(
            "there's no partition called \"{name}\" here. Try: {}",
            scan.partitions.iter().map(|p| p.id()).collect::<Vec<_>>().join(", ")
        ))
    })?;
    if !found.can_mount() {
        return Err(CliError::Usage(if found.spec.is_fatx() {
            format!("{} has no FATX volume on it yet. Format it first.", found.name())
        } else {
            format!("{} isn't a FATX partition and can't be opened.", found.name())
        }));
    }
    Ok(found.clone())
}

/// A target, its detected partitions, and one opened volume.
pub struct Session {
    pub partition: DetectedPartition,
    pub volume: FatxVolume,
}

impl Session {
    pub fn open(spec: &str, partition: Option<&str>, write: bool, allow_pc_disk: bool) -> Result<Session, CliError> {
        let target = Target::open(spec, write, allow_pc_disk)?;
        let scan = scanner::scan(target.device.as_ref())?;
        let partition = pick(&scan, partition)?;
        let mut volume = scanner::open_partition(&target.device, &partition, write)?;
        if write {
            volume.scan_free_space(|_| {})?;
        }
        Ok(Session { partition, volume })
    }
}

/// The first word after the target may be a partition name or already the path. Returns the
/// partition name (if it was one) and the words that are left.
pub fn split_partition(positional: &[String]) -> (Option<String>, Vec<String>) {
    let Some(first) = positional.first() else {
        return (None, Vec::new());
    };
    let looks_like_path = first.contains(['\\', '/', '.', ':']);
    if !looks_like_path && is_partition_name(first) {
        return (Some(first.clone()), positional[1..].to_vec());
    }
    (None, positional.to_vec())
}

fn is_partition_name(text: &str) -> bool {
    if text.eq_ignore_ascii_case(scanner::SINGLE_PARTITION_ID) {
        return true;
    }
    [
        layout::CONTENT_ID,
        layout::COMPAT_ID,
        layout::SYSTEM_EXT_ID,
        layout::SYSTEM_EXT2_ID,
        layout::SYSTEM_CACHE_ID,
        layout::GAME_CACHE_ID,
    ]
    .contains(&layout::normalize(text).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_partition_name_is_split_off() {
        let (partition, rest) = split_partition(&words(&["content", "\\Content"]));
        assert_eq!(partition.as_deref(), Some("content"));
        assert_eq!(rest, ["\\Content"]);
        let (partition, _) = split_partition(&words(&["Data"]));
        assert_eq!(partition.as_deref(), Some("Data"));
    }

    #[test]
    fn a_path_is_left_alone() {
        for first in ["\\Content", "/Content", "save.bin", "C:"] {
            let (partition, rest) = split_partition(&words(&[first, "x"]));
            assert!(partition.is_none(), "{first}");
            assert_eq!(rest.len(), 2);
        }
        let (partition, rest) = split_partition(&words(&["Games"]));
        assert!(partition.is_none());
        assert_eq!(rest, ["Games"]);
    }
}
