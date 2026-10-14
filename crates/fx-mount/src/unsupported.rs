//! Until FUSE is hooked up there's nothing to mount with here.

use fx_core::fatx::FatxVolume;

use crate::MountError;

pub const DRIVER_NAME: &str = "FUSE";
pub const DRIVER_PAGE: &str = "https://github.com/libfuse/libfuse";

pub struct Mounted;

impl Mounted {
    pub fn mount_point(&self) -> &str {
        ""
    }

    pub fn read_only(&self) -> bool {
        true
    }

    pub fn unmount(self) -> Result<(), MountError> {
        Ok(())
    }
}

pub fn mount(_volume: FatxVolume, _label: &str, _point: Option<&str>) -> Result<Mounted, MountError> {
    Err(MountError("mounting isn't supported on this system yet".into()))
}

pub fn driver_installed() -> bool {
    false
}

pub fn driver_version() -> Option<String> {
    None
}
