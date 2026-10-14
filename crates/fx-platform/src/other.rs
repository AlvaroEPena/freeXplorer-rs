//! Placeholder until Linux and macOS are filled in: no disks are found, image files still work.

use std::io;
use std::path::Path;

use fx_core::backup::DestinationInfo;
use fx_core::device::Device;

use crate::DiskInfo;

pub fn normalize_id(_rest: &str) -> Option<String> {
    None
}

pub fn parse_path(_spec: &str) -> Option<String> {
    None
}

pub fn list_disks() -> Vec<DiskInfo> {
    Vec::new()
}

pub fn open_disk(disk: &DiskInfo, _writable: bool) -> io::Result<Device> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("opening disk {} isn't supported on this system yet", disk.id),
    ))
}

pub fn is_elevated() -> bool {
    false
}

pub fn destination_info(_path: &Path) -> DestinationInfo {
    DestinationInfo::default()
}

pub struct Awake;

impl Awake {
    pub fn new(_reason: &str) -> Awake {
        Awake
    }
}
