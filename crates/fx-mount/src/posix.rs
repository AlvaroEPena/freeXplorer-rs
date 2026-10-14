//! Mounting on Linux and macOS: the partition shows up as a folder, through FUSE.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fuser::{BackgroundSession, Config, MountOption, SessionACL};
use fx_core::fatx::FatxVolume;

use crate::MountError;
use crate::fuse::FatxFuse;

#[cfg(target_os = "macos")]
pub const DRIVER_NAME: &str = "macFUSE";
#[cfg(not(target_os = "macos"))]
pub const DRIVER_NAME: &str = "FUSE";

#[cfg(target_os = "macos")]
pub const DRIVER_PAGE: &str = "https://macfuse.github.io";
#[cfg(not(target_os = "macos"))]
pub const DRIVER_PAGE: &str = "https://github.com/libfuse/libfuse";

#[cfg(not(target_os = "macos"))]
fn in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .chain(["/usr/bin", "/bin", "/usr/local/bin", "/sbin", "/usr/sbin"].map(PathBuf::from))
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

#[cfg(target_os = "macos")]
pub fn driver_installed() -> bool {
    Path::new("/Library/Filesystems/macfuse.fs").is_dir()
}

#[cfg(not(target_os = "macos"))]
pub fn driver_installed() -> bool {
    Path::new("/dev/fuse").exists() && (in_path("fusermount3").is_some() || in_path("fusermount").is_some())
}

#[cfg(target_os = "macos")]
pub fn driver_version() -> Option<String> {
    driver_installed().then(|| "4".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn driver_version() -> Option<String> {
    if !driver_installed() {
        return None;
    }
    // "fusermount3 version: 3.10.5"
    let program = in_path("fusermount3").or_else(|| in_path("fusermount"))?;
    let output = std::process::Command::new(program).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.split_whitespace().last().unwrap_or("").to_string())
}

pub struct Mounted {
    session: Option<BackgroundSession>,
    volume: Arc<Mutex<FatxVolume>>,
    point: PathBuf,
    read_only: bool,
    /// a folder this program made for the mount, taken away again when it's unmounted
    made: bool,
}

impl Mounted {
    pub fn mount_point(&self) -> &str {
        self.point.to_str().unwrap_or("")
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn unmount(mut self) -> Result<(), MountError> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<(), MountError> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        let unmounted = session
            .umount_and_join()
            .map_err(|e| MountError(format!("couldn't unmount {}: {e}", self.point.display())));
        if self.made {
            // only goes if it's empty, which it is once nothing is mounted on it
            let _ = std::fs::remove_dir(&self.point);
        }
        unmounted?;
        if self.read_only {
            return Ok(());
        }
        let mut volume = self.volume.lock().unwrap_or_else(|p| p.into_inner());
        volume
            .flush()
            .map_err(|e| MountError(format!("couldn't save the last changes: {e}")))
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = self.shut_down();
    }
}

fn home_of_user() -> Option<PathBuf> {
    // under sudo the folder belongs to the person who ran it, not to root
    if let Some(user) = std::env::var_os("SUDO_USER").filter(|u| u != "root") {
        let base = if cfg!(target_os = "macos") { "/Users" } else { "/home" };
        let home = Path::new(base).join(user);
        if home.is_dir() {
            return Some(home);
        }
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

/// A folder that can't clash with a file system's own name: letters, digits, spaces and dashes.
fn folder_name(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('-');
    if trimmed.is_empty() {
        "partition".into()
    } else {
        trimmed.to_string()
    }
}

fn default_folder(label: &str) -> Result<PathBuf, MountError> {
    let home = home_of_user()
        .ok_or_else(|| MountError("Can't tell where to mount it. Give a folder with --folder.".into()))?;
    let base = home.join("freeXplorer-rs");
    let name = folder_name(label);
    // a second mount of a partition with the same label gets a number
    for n in 1..100 {
        let candidate = if n == 1 {
            base.join(&name)
        } else {
            base.join(format!("{name} {n}"))
        };
        let empty = std::fs::read_dir(&candidate)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
        if empty {
            return Ok(candidate);
        }
    }
    Err(MountError(format!(
        "Too many folders called {name} already. Give a folder with --folder."
    )))
}

pub fn mount(volume: FatxVolume, label: &str, point: Option<&str>) -> Result<Mounted, MountError> {
    if !driver_installed() {
        return Err(MountError(format!(
            "{DRIVER_NAME} isn't installed, and mounting needs it. See {DRIVER_PAGE}."
        )));
    }
    let read_only = volume.is_read_only();
    let folder = match point.map(str::trim).filter(|p| !p.is_empty() && *p != "*") {
        Some(p) => PathBuf::from(p),
        None => default_folder(label)?,
    };
    let existed = folder.exists();
    std::fs::create_dir_all(&folder)
        .map_err(|e| MountError(format!("Couldn't make the folder {}: {e}", folder.display())))?;
    if std::fs::read_dir(&folder)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        return Err(MountError(format!(
            "{} isn't empty, mount onto an empty folder.",
            folder.display()
        )));
    }
    // when root made the folder under sudo, hand it to the person who asked
    #[cfg(unix)]
    if !existed && let Some(owner) = sudo_owner() {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c_path) = std::ffi::CString::new(folder.as_os_str().as_bytes()) {
            // SAFETY: nul terminated path
            unsafe { libc::chown(c_path.as_ptr(), owner.0, owner.1) };
        }
    }

    // SAFETY: no arguments
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let volume = Arc::new(Mutex::new(volume));
    let filesystem = FatxFuse::new(Arc::clone(&volume), uid, gid);

    let mut options = vec![
        MountOption::FSName("freeXplorer-rs".into()),
        MountOption::Subtype("fatx".into()),
    ];
    options.push(if read_only { MountOption::RO } else { MountOption::RW });
    let mut config = Config::default();
    config.mount_options = options;
    // root's mount has to be usable by the person who started it, who isn't root
    if uid == 0 {
        config.acl = SessionACL::All;
    }

    let session = fuser::spawn_mount(filesystem, &folder, &config).map_err(|e| {
        if !existed {
            let _ = std::fs::remove_dir(&folder);
        }
        MountError(format!("Couldn't mount on {}: {e}", folder.display()))
    })?;
    Ok(Mounted {
        session: Some(session),
        volume,
        point: folder,
        read_only,
        made: !existed,
    })
}

/// The user and group behind a sudo, if this is one.
fn sudo_owner() -> Option<(libc::uid_t, libc::gid_t)> {
    let uid = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    let gid = std::env::var("SUDO_GID").ok()?.parse().ok()?;
    Some((uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_kept_plain() {
        assert_eq!(folder_name("Xbox Games"), "Xbox Games");
        assert_eq!(folder_name("a/b:c"), "a-b-c");
        assert_eq!(folder_name("///"), "partition");
        assert_eq!(folder_name(""), "partition");
    }
}
