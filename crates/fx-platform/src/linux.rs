//! Linux: disks come from /sys, and are opened as /dev/sdb and the like.

use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use fx_core::backup::DestinationInfo;
use fx_core::device::Device;

use crate::DiskInfo;
use crate::raw::open_raw;
use crate::sysfs::{self, Roots};
use crate::unix;

pub use crate::unix::Awake;

/// "sdb", "nvme0n1", "mmcblk0": letters and digits, no slashes.
pub fn normalize_id(rest: &str) -> Option<String> {
    let name = rest.trim().trim_start_matches("/dev/");
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| name.to_string())
}

pub fn parse_path(spec: &str) -> Option<String> {
    spec.trim().strip_prefix("/dev/").and_then(normalize_id)
}

pub fn list_disks() -> Vec<DiskInfo> {
    sysfs::list(&Roots::real())
}

pub fn open_disk(disk: &DiskInfo, writable: bool) -> io::Result<Device> {
    // The disk could have been swapped since the list was made. A USB adapter keeps its name whichever
    // drive is inside it, so the size is what tells the drives apart.
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
    open_raw(
        &now.path,
        &format!("disk {}", now.id),
        now.length,
        now.sector_size,
        writable,
        |options| {
            // exclusive: the kernel refuses a write open of a disk that something else is using
            if writable {
                options.custom_flags(libc::O_EXCL);
            }
        },
    )
}

pub fn is_elevated() -> bool {
    unix::is_root()
}

pub fn destination_info(path: &Path) -> DestinationInfo {
    unix::destination_info(path)
}
