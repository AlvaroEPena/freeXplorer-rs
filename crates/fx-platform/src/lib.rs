//! The operating system specific parts: finding the disks that are connected, opening one raw,
//! and a few small things a long job wants from the OS.

use std::io;
use std::path::Path;

use fx_core::backup::DestinationInfo;
use fx_core::device::Device;
use fx_core::size;

mod raw;
pub mod safety;

// These are plain text and file reading, so they build everywhere and the tests run on any system.
// Only the Linux and macOS code below uses them for real.
#[allow(dead_code)]
mod diskutil;
#[allow(dead_code)]
mod fsinfo;
#[allow(dead_code)]
mod sysfs;

#[cfg(unix)]
mod unix;

#[cfg(unix)]
mod driver;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as sys;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as sys;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod other;
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
use other as sys;

/// A disk as the operating system lists it. `id` is what the user types after "disk:", a number
/// on Windows (3), a device name elsewhere (sdb, disk2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub id: String,
    pub path: String,
    pub length: u64,
    pub sector_size: u32,
    pub model: String,
    pub serial: String,
    pub bus: String,
    pub removable: bool,
    /// a file system on it is mounted, so it's a normal computer disk and not a console drive
    pub has_volumes: bool,
    /// the disk the operating system is running from
    pub is_system: bool,
}

impl DiskInfo {
    pub fn display(&self) -> String {
        let model = if self.model.trim().is_empty() {
            "Unknown drive"
        } else {
            self.model.as_str()
        };
        format!(
            "Disk {}: {} ({}, {})",
            self.id,
            model,
            size::format(self.length),
            self.bus
        )
    }
}

/// Reads "disk:3", or whatever else this OS calls a disk by (\\.\PhysicalDrive3, /dev/sdb).
pub fn parse_disk_spec(spec: &str) -> Option<String> {
    let spec = spec.trim();
    match spec.get(..5) {
        Some(prefix) if prefix.eq_ignore_ascii_case("disk:") => sys::normalize_id(&spec[5..]),
        _ => sys::parse_path(spec),
    }
}

/// Looks at every disk without reading from it, so it doesn't need elevation.
pub fn list_disks() -> Vec<DiskInfo> {
    sys::list_disks()
}

/// Opens a disk for raw access. Writing is only allowed by `safety::check_write` and that is
/// the caller's job, this does what it's told.
pub fn open_disk(disk: &DiskInfo, writable: bool) -> io::Result<Device> {
    sys::open_disk(disk, writable)
}

/// Whether the process can open raw disks without further ado.
pub fn is_elevated() -> bool {
    sys::is_elevated()
}

/// What `fx_core::backup::check_destination` needs to know about the place a file is going.
pub fn destination_info(path: &Path) -> DestinationInfo {
    sys::destination_info(path)
}

/// Shows a folder (or opens a web address) in whatever the system uses for that.
pub fn open_in_shell(target: &str) {
    use std::process::Command;
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("explorer.exe");
        c.arg(target).creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("open");
        c.arg(target);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    let _ = command.spawn();
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const DRIVER_PAGE: &str = "https://winfsp.dev/rel/";

/// Installs what mounting needs, where the system has a package manager that can do it. Waits for
/// it to finish. The error is something to show as it is.
pub fn install_driver_package() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // winget says this when the package is there already
        const ALREADY_INSTALLED: i32 = 0x8A15_002Bu32 as i32;
        let output = std::process::Command::new("winget")
            .args([
                "install",
                "-e",
                "--id",
                "WinFsp.WinFsp",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|_| {
                format!(
                    "winget isn't available here, so the driver can't be installed from the window. Download WinFsp from {DRIVER_PAGE} and install it yourself."
                )
            })?;
        match output.status.code() {
            Some(0) | Some(ALREADY_INSTALLED) => Ok(()),
            _ => {
                let text = String::from_utf8_lossy(&output.stdout);
                let tail: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(3).collect();
                Err(format!(
                    "The install didn't finish ({}). {}\nYou can also download WinFsp from {DRIVER_PAGE}.",
                    output.status,
                    tail.into_iter().rev().collect::<Vec<_>>().join(" ")
                ))
            }
        }
    }
    #[cfg(unix)]
    {
        driver::install()
    }
    #[cfg(not(any(windows, unix)))]
    Err("Install FUSE with your package manager (for example: sudo apt install fuse3).".to_string())
}

/// Keeps the computer from going to sleep in the middle of a backup while this is alive.
pub struct KeepAwake {
    _request: sys::Awake,
}

impl KeepAwake {
    pub fn new(reason: &str) -> KeepAwake {
        KeepAwake {
            _request: sys::Awake::new(reason),
        }
    }
}
