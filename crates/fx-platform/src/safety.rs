//! The rules for letting this program write to a real disk. Reading is always allowed.

use crate::DiskInfo;

pub fn check_write(disk: &DiskInfo, allow_pc_disk: bool) -> Result<(), String> {
    if disk.is_system {
        return Err(format!(
            "Disk {} is the disk this computer is running from. This program will never write to it.",
            disk.id
        ));
    }
    if disk.has_volumes && !allow_pc_disk {
        return Err(format!(
            "Disk {} has a file system mounted from it, so it looks like a normal computer disk and not an Xbox 360 drive. \
             If you're sure, pass --allow-pc-disk to the command line tool.",
            disk.id
        ));
    }
    Ok(())
}

/// What the user has to type before something destructive starts.
pub fn confirmation_phrase(disk: &DiskInfo) -> String {
    format!("disk {}", disk.id)
}

pub fn is_confirmed(disk: &DiskInfo, typed: Option<&str>) -> bool {
    typed.is_some_and(|t| t.trim().eq_ignore_ascii_case(&confirmation_phrase(disk)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(system: bool, volumes: bool) -> DiskInfo {
        DiskInfo {
            id: "3".into(),
            path: String::new(),
            length: 1 << 30,
            sector_size: 512,
            model: "Test".into(),
            serial: String::new(),
            bus: "USB".into(),
            removable: true,
            has_volumes: volumes,
            is_system: system,
        }
    }

    #[test]
    fn the_system_disk_is_never_allowed() {
        assert!(check_write(&disk(true, true), true).is_err());
        assert!(check_write(&disk(true, false), true).is_err());
    }

    #[test]
    fn a_disk_with_volumes_needs_the_override() {
        assert!(check_write(&disk(false, true), false).is_err());
        assert!(check_write(&disk(false, true), true).is_ok());
        assert!(check_write(&disk(false, false), false).is_ok());
    }

    #[test]
    fn confirmation_ignores_case_and_padding() {
        let d = disk(false, false);
        assert!(is_confirmed(&d, Some("  Disk 3 ")));
        assert!(!is_confirmed(&d, Some("disk 4")));
        assert!(!is_confirmed(&d, Some("yes")));
        assert!(!is_confirmed(&d, None));
    }
}
