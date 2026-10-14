//! Mounting through WinFsp, the free driver that lets a user mode program be a Windows drive.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_core::fatx::FatxVolume;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, RRF_RT_REG_SZ,
    RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
};
use winfsp::host::{FileSystemHost, VolumeParams};

use crate::MountError;
use fs::{FatxFs, OPEN_SECURITY};

mod fs;

const NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
const ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
const DRIVER_NOT_RUNNING: i32 = 0xC000_000Fu32 as i32;
/// HRESULTs that come from an NTSTATUS have this bit set on top of it
const NT_FACILITY_BIT: u32 = 0x1000_0000;

pub const DRIVER_NAME: &str = "WinFsp";
pub const DRIVER_PAGE: &str = "https://winfsp.dev/rel/";

/// Where WinFsp put its library, from the registry, whichever view of it has the key.
fn installed_library() -> Option<std::path::PathBuf> {
    let key_path = wide(r"SOFTWARE\WinFsp");
    let value = wide("InstallDir");
    let dll = if cfg!(target_arch = "x86_64") {
        "winfsp-x64.dll"
    } else if cfg!(target_arch = "aarch64") {
        "winfsp-a64.dll"
    } else {
        "winfsp-x86.dll"
    };
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        // SAFETY: the strings are nul terminated, the key is closed again and the size is in bytes
        let dir = unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, key_path.as_ptr(), 0, KEY_READ | view, &mut key) != 0 {
                continue;
            }
            let mut buffer = vec![0u16; 520];
            let mut bytes = (buffer.len() * 2) as u32;
            let status = RegGetValueW(
                key,
                std::ptr::null(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            );
            RegCloseKey(key);
            if status != 0 {
                continue;
            }
            let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..length]))
        };
        let path = dir.join("bin").join(dll);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Loads the WinFsp library, which is looked for only now and not when the program starts.
///
/// The library is loaded from where WinFsp installed itself. After that the loading the crate does
/// by name finds it already there.
fn driver_loaded() -> bool {
    static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LOADED.get_or_init(|| {
        if let Some(path) = installed_library() {
            let wide_path = wide(&path.to_string_lossy());
            // SAFETY: nul terminated path; the library is meant to stay loaded
            unsafe { LoadLibraryW(wide_path.as_ptr()) };
        }
        winfsp::winfsp_init().is_ok()
    })
}

pub fn driver_installed() -> bool {
    driver_loaded()
}

/// "2.1", or None when WinFsp isn't installed.
pub fn driver_version() -> Option<String> {
    if !driver_loaded() {
        return None;
    }
    let mut raw = 0u32;
    // SAFETY: the library is loaded, and the pointer is to a local the call fills in
    (unsafe { winfsp_sys::FspVersion(&mut raw) } >= 0).then(|| format!("{}.{}", raw >> 16, raw & 0xFFFF))
}

pub struct Mounted {
    host: Option<FileSystemHost<FatxFs>>,
    volume: Arc<Mutex<FatxVolume>>,
    point: String,
    read_only: bool,
}

impl Mounted {
    pub fn mount_point(&self) -> &str {
        &self.point
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn unmount(mut self) -> Result<(), MountError> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<(), MountError> {
        // dropping the host takes the mount point away, stops the dispatcher and frees the file system
        if self.host.take().is_none() || self.read_only {
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

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn open_security() -> Vec<u8> {
    let sddl = wide(OPEN_SECURITY);
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let mut size = 0u32;
    // SAFETY: nul terminated string, outputs are locals; the buffer Windows allocates is copied and freed
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, &mut size) == 0 {
            return Vec::new();
        }
        let copy = std::slice::from_raw_parts(descriptor.cast::<u8>(), size as usize).to_vec();
        LocalFree(descriptor);
        copy
    }
}

fn file_time_now() -> u64 {
    let since_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (since_unix.as_secs() + 11_644_473_600) * 10_000_000
}

/// The NTSTATUS inside an error from WinFsp's host, which hands them over as HRESULTs.
fn status_of(code: i32) -> i32 {
    (code as u32 & !NT_FACILITY_BIT) as i32
}

pub fn mount(volume: FatxVolume, label: &str, point: Option<&str>) -> Result<Mounted, MountError> {
    if !driver_loaded() {
        return Err(MountError(format!(
            "{DRIVER_NAME} isn't installed. It's the free driver that lets Windows show a FATX partition as a drive. \
             Get it from {DRIVER_PAGE} and run this again."
        )));
    }

    let label: String = label.chars().take(32).collect();
    let (read_only, cluster_size, serial) = (volume.is_read_only(), volume.cluster_size(), volume.volume_id());
    let volume = Arc::new(Mutex::new(volume));
    let context = FatxFs::new(Arc::clone(&volume), label, open_security());

    let mut params = VolumeParams::new();
    params
        .sector_size(512)
        .sectors_per_allocation_unit((cluster_size / 512) as u16)
        .max_component_length(fx_core::fatx::name::MAX_LENGTH as u16)
        .volume_creation_time(file_time_now())
        .volume_serial_number(serial)
        .file_info_timeout(1000)
        .case_preserved_names(true)
        .post_cleanup_when_modified_only(true)
        .filesystem_name("FATX");
    let mut host =
        FileSystemHost::new(params, context).map_err(|e| MountError(describe(status_of(e.code().0), "the volume")))?;

    let (letter, folder) = parse_mount_point(point);
    let chosen = if let Some(folder) = folder {
        try_mount(&mut host, std::slice::from_ref(&folder)).map_err(|s| describe(s, &folder))
    } else {
        let automatic = letter.is_none();
        let letters: Vec<char> = match letter {
            Some(c) => vec![c],
            None => free_drive_letters(&taken_letters()).into_iter().take(4).collect(),
        };
        if letters.is_empty() {
            return Err(MountError(
                "Every drive letter is taken. Unmount something, or mount into a folder.".into(),
            ));
        }
        // Drive letters go through the mount manager, which makes a real disk that This PC and every
        // program sees. A plain letter made by an administrator is private to that one elevated session,
        // so that is only the second choice, for when the first is refused.
        let mut outcome = Err(NAME_COLLISION);
        for c in letters {
            outcome = try_mount(&mut host, &[format!(r"\\.\{c}:"), format!("{c}:")]);
            // someone else grabbed the letter between looking and mounting, so try the next one
            if outcome.is_ok() || !automatic || outcome != Err(NAME_COLLISION) {
                break;
            }
        }
        let wanted = letter.map_or_else(|| "a drive letter".to_string(), |c| format!("{c}:"));
        outcome.map_err(|s| describe(s, &wanted))
    };
    let used = chosen.map_err(MountError)?;

    host.start()
        .map_err(|e| MountError(describe(status_of(e.code().0), "the volume")))?;
    Ok(Mounted {
        host: Some(host),
        volume,
        point: display_name(&used),
        read_only,
    })
}

/// Tries each way of naming the mount point in turn, and gives back the one that worked. A refusal
/// of the first (access denied) is the only thing that makes it go on to the next.
fn try_mount(host: &mut FileSystemHost<FatxFs>, points: &[String]) -> Result<String, i32> {
    let mut status = ACCESS_DENIED;
    for point in points {
        match host.mount(point) {
            Ok(()) => return Ok(point.clone()),
            Err(e) => status = status_of(e.code().0),
        }
        if status != ACCESS_DENIED {
            break;
        }
    }
    Err(status)
}

fn describe(status: i32, whence: &str) -> String {
    match status {
        NAME_COLLISION => format!("{whence} is already in use."),
        ACCESS_DENIED => format!("Windows refused to mount on {whence}. Try another letter, or run as administrator."),
        DRIVER_NOT_RUNNING => "WinFsp's driver isn't running. Reboot, or reinstall WinFsp.".into(),
        other => format!("WinFsp couldn't mount the volume (error 0x{:08X}).", other as u32),
    }
}

/// null or "*" means any free letter, "X", "X:" and \\.\X: name one, the rest is a folder
pub(crate) fn parse_mount_point(point: Option<&str>) -> (Option<char>, Option<String>) {
    let Some(raw) = point.map(str::trim).filter(|p| !p.is_empty() && *p != "*") else {
        return (None, None);
    };
    let mut p = raw;
    if p.starts_with(r"\\.\") || p.starts_with(r"\\?\") {
        p = &p[4..];
    }
    if p.len() > 1 {
        p = p.strip_suffix('\\').unwrap_or(p);
    }
    let mut chars = p.chars();
    let first = chars.next();
    let rest: String = chars.collect();
    match first {
        Some(c) if c.is_ascii_alphabetic() && (rest.is_empty() || rest == ":") => (Some(c.to_ascii_uppercase()), None),
        _ => (None, Some(raw.to_string())),
    }
}

/// what to show for a mount point: "X:" for any of the spellings of a drive letter
fn display_name(point: &str) -> String {
    match parse_mount_point(Some(point)).0 {
        Some(c) => format!("{c}:"),
        None => point.to_string(),
    }
}

/// From Z down, which keeps clear of the letters USB sticks and card readers like to take.
pub(crate) fn free_drive_letters(taken: &HashSet<char>) -> Vec<char> {
    "ZYXWVUTSRQPONMLKJIHGFED"
        .chars()
        .filter(|c| !taken.contains(c))
        .collect()
}

fn taken_letters() -> HashSet<char> {
    let mut taken = HashSet::new();
    // SAFETY: no arguments
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) != 0 {
            taken.insert((b'A' + i as u8) as char);
        }
    }

    // letters Windows keeps for disks that just aren't plugged in at the moment (only administrators can read this)
    for name in registry_names(HKEY_LOCAL_MACHINE, r"SYSTEM\MountedDevices", true) {
        let upper = name.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix(r"\DOSDEVICES\")
            && let [letter, b':'] = rest.as_bytes()
            && letter.is_ascii_alphabetic()
        {
            taken.insert(*letter as char);
        }
    }

    // network drives that come back at every logon. An elevated program can't see those through
    // GetLogicalDrives, they belong to the other half of the user's session.
    for name in registry_names(HKEY_CURRENT_USER, "Network", false) {
        if let [letter] = name.as_bytes()
            && letter.is_ascii_alphabetic()
        {
            taken.insert(letter.to_ascii_uppercase() as char);
        }
    }
    taken
}

fn registry_names(root: HKEY, path: &str, values: bool) -> Vec<String> {
    let path = wide(path);
    let mut names = Vec::new();
    // SAFETY: the key is closed again, buffers are sized in characters as the calls expect
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return names;
        }
        for index in 0.. {
            let mut buffer = [0u16; 512];
            let mut length = buffer.len() as u32;
            let status = if values {
                RegEnumValueW(
                    key,
                    index,
                    buffer.as_mut_ptr(),
                    &mut length,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } else {
                RegEnumKeyExW(
                    key,
                    index,
                    buffer.as_mut_ptr(),
                    &mut length,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                break;
            }
            names.push(String::from_utf16_lossy(&buffer[..length as usize]));
        }
        RegCloseKey(key);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_are_told_apart() {
        assert_eq!(parse_mount_point(None), (None, None));
        assert_eq!(parse_mount_point(Some("*")), (None, None));
        assert_eq!(parse_mount_point(Some("x")), (Some('X'), None));
        assert_eq!(parse_mount_point(Some("x:")), (Some('X'), None));
        assert_eq!(parse_mount_point(Some(r"\\.\q:")), (Some('Q'), None));
        assert_eq!(
            parse_mount_point(Some(r"C:\mnt\xbox")),
            (None, Some(r"C:\mnt\xbox".to_string()))
        );
        assert_eq!(display_name(r"\\.\q:"), "Q:");
        assert_eq!(display_name(r"C:\mnt"), r"C:\mnt");
    }

    #[test]
    fn letters_count_down_from_z_and_skip_taken_ones() {
        let taken: HashSet<char> = ['Z', 'Y', 'C'].into();
        let free = free_drive_letters(&taken);
        assert_eq!(free[0], 'X');
        assert!(!free.contains(&'C'));
        assert!(!free.contains(&'A'));
    }

    #[test]
    fn host_errors_give_back_the_status_inside() {
        assert_eq!(status_of(0xD000_0035u32 as i32), NAME_COLLISION);
        assert_eq!(status_of(NAME_COLLISION), NAME_COLLISION);
    }
}
