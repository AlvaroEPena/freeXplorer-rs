//! Makes a FATX partition show up as a drive or a folder in the file manager. Windows does that
//! through WinFsp; Linux and macOS will use FUSE.

use std::fmt;

use fx_core::fatx::FatxVolume;

#[cfg(windows)]
mod winfsp;
#[cfg(windows)]
use winfsp as sys;

#[cfg(unix)]
mod fuse;
#[cfg(unix)]
mod posix;
#[cfg(unix)]
use posix as sys;

#[cfg(not(any(windows, unix)))]
mod unsupported;
#[cfg(not(any(windows, unix)))]
use unsupported as sys;

#[derive(Debug)]
pub struct MountError(pub String);

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MountError {}

/// A volume that is currently showing up on the computer. Dropping it unmounts it.
pub struct Mounted(sys::Mounted);

impl Mounted {
    /// "X:" for a drive letter, the folder's path otherwise
    pub fn mount_point(&self) -> &str {
        self.0.mount_point()
    }

    pub fn read_only(&self) -> bool {
        self.0.read_only()
    }

    /// Takes the drive away and saves what was left unwritten.
    pub fn unmount(self) -> Result<(), MountError> {
        self.0.unmount()
    }
}

/// Mounts a volume. `point` is a drive letter on Windows ("X:", or None for the next free one
/// counting down from Z) and an empty folder on the other systems.
pub fn mount(volume: FatxVolume, label: &str, point: Option<&str>) -> Result<Mounted, MountError> {
    sys::mount(volume, label, point).map(Mounted)
}

/// What mounting needs from the system, and the page to get it from.
pub const DRIVER_NAME: &str = sys::DRIVER_NAME;
pub const DRIVER_PAGE: &str = sys::DRIVER_PAGE;

pub fn driver_installed() -> bool {
    sys::driver_installed()
}

pub fn driver_version() -> Option<String> {
    sys::driver_version()
}
