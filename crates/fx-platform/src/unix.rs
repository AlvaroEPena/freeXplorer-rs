//! The parts Linux and macOS have in common.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use fx_core::backup::DestinationInfo;

use crate::fsinfo;

pub fn is_root() -> bool {
    // SAFETY: no arguments, can't fail
    unsafe { libc::geteuid() == 0 }
}

/// The closest folder to this path that exists, which is where the room can be asked about.
fn existing_ancestor(path: &Path) -> PathBuf {
    let mut current = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    while !current.exists() {
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    current
}

fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: a zeroed statvfs is a valid value for the call to fill in, and the path is nul terminated
    unsafe {
        let mut stats: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stats) != 0 {
            return None;
        }
        Some(stats.f_bavail as u64 * stats.f_frsize as u64)
    }
}

/// The file system a path is on, and where it is mounted.
#[cfg(target_os = "linux")]
fn file_system(path: &Path) -> Option<(String, String)> {
    let mounts = crate::sysfs::mounts(&crate::sysfs::Roots::real());
    let full = std::fs::canonicalize(path).ok()?;
    mounts
        .into_iter()
        .filter(|m| full.starts_with(&m.mount_point))
        .max_by_key(|m| m.mount_point.len())
        .map(|m| (m.fs_type, m.mount_point))
}

#[cfg(target_os = "macos")]
fn file_system(path: &Path) -> Option<(String, String)> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: a zeroed statfs is a valid value for the call to fill in, and the path is nul terminated
    unsafe {
        let mut stats: libc::statfs = std::mem::zeroed();
        if libc::statfs(c_path.as_ptr(), &mut stats) != 0 {
            return None;
        }
        let name = |raw: &[libc::c_char]| {
            let bytes: Vec<u8> = raw.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Some((name(&stats.f_fstypename), name(&stats.f_mntonname)))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn file_system(_path: &Path) -> Option<(String, String)> {
    None
}

pub fn destination_info(path: &Path) -> DestinationInfo {
    let place = existing_ancestor(path);
    let (file_system, root) = file_system(&place).unwrap_or_default();
    let traits = fsinfo::classify(&file_system);
    DestinationInfo {
        root: if root.is_empty() {
            place.to_string_lossy().into_owned()
        } else {
            root
        },
        free_bytes: free_bytes(&place),
        supports_sparse: traits.sparse,
        max_file_size: traits.max_file_size,
        network: traits.network,
        file_system,
    }
}

/// Keeps the computer awake by running a program that does that for as long as it lives. If the
/// system hasn't got one the work just goes ahead without it.
pub struct Awake(Option<Child>);

impl Awake {
    pub fn new(reason: &str) -> Awake {
        #[cfg(target_os = "macos")]
        let command = Command::new("caffeinate")
            .arg("-i")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        #[cfg(not(target_os = "macos"))]
        let command = Command::new("systemd-inhibit")
            .args([
                "--what=sleep:idle",
                "--who=freeXplorer-rs",
                &format!("--why={reason}"),
                "--mode=block",
                "sleep",
                "infinity",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        #[cfg(target_os = "macos")]
        let _ = reason;
        Awake(command.ok())
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
