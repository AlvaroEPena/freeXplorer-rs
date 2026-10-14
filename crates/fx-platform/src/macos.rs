//! macOS: disks come from `diskutil`, and are opened as /dev/rdisk4, the raw device.
//!
//! Written without a Mac to run it on, only checked against saved `diskutil` output and compiled.

use std::io;
use std::path::Path;
use std::process::Command;

use fx_core::backup::DestinationInfo;
use fx_core::device::Device;

use crate::DiskInfo;
use crate::diskutil;
use crate::raw::open_raw;
use crate::unix;

pub use crate::unix::Awake;

/// "disk4", from "disk4", "4" or "rdisk4".
pub fn normalize_id(rest: &str) -> Option<String> {
    let name = rest.trim().trim_start_matches("/dev/");
    let name = name.strip_prefix('r').filter(|n| n.starts_with("disk")).unwrap_or(name);
    let digits = name.strip_prefix("disk").unwrap_or(name);
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then(|| format!("disk{digits}"))
}

pub fn parse_path(spec: &str) -> Option<String> {
    spec.trim().strip_prefix("/dev/").and_then(normalize_id)
}

fn diskutil(args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("diskutil").args(args).output().ok()?;
    output.status.success().then_some(output.stdout)
}

pub fn list_disks() -> Vec<DiskInfo> {
    diskutil::list(&diskutil)
}

pub fn open_disk(disk: &DiskInfo, writable: bool) -> io::Result<Device> {
    let now = list_disks().into_iter().find(|d| d.id == disk.id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("Disk {} isn't connected any more.", disk.id),
        )
    })?;
    if now.length != disk.length || now.sector_size != disk.sector_size {
        return Err(io::Error::other(format!(
            "Disk {} is a different drive now. It was {} bytes when the list was loaded and it is {} bytes now.",
            disk.id, disk.length, now.length
        )));
    }
    // macOS won't let anything write to a disk that has volumes mounted from it
    if writable && now.has_volumes {
        let unmounted = Command::new("diskutil")
            .args(["unmountDisk", &format!("/dev/{}", now.id)])
            .output()
            .is_ok_and(|o| o.status.success());
        if !unmounted {
            return Err(io::Error::other(format!(
                "Disk {} is in use and macOS wouldn't let go of it. Unmount its volumes and try again.",
                now.id
            )));
        }
    }
    open_raw(&now.path, &format!("disk {}", now.id), now.length, now.sector_size, writable, |_| {}).map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "macOS won't let this program open {}. Run it with sudo, and give your terminal Full Disk Access in System Settings > Privacy & Security.",
                    now.id
                ),
            )
        } else {
            e
        }
    })
}

pub fn is_elevated() -> bool {
    unix::is_root()
}

pub fn destination_info(path: &Path) -> DestinationInfo {
    unix::destination_info(path)
}
