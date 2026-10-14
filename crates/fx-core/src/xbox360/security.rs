use super::layout::{self, SECURITY_SECTOR_OFFSET, SYSTEM_CACHE_OFFSET};
use crate::device::{BlockDevice, read_vec};
use crate::error::Result;
use crate::size;

/// What a console drive says about itself in its security sector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecuritySector {
    pub serial: String,
    pub claimed_length: u64,
}

/// The start of a drive that belongs to the drive itself. The security sector is in here and
/// the first partition begins right after it.
pub const HEADER_LENGTH: u64 = SYSTEM_CACHE_OFFSET;

/// The serial number is the first 0x14 bytes, and the number of sectors the console may use is at +0x58.
pub fn read(device: &dyn BlockDevice) -> Result<Option<SecuritySector>> {
    if device.len() < SECURITY_SECTOR_OFFSET + 0x5C || !layout::looks_like_security_sector(device)? {
        return Ok(None);
    }
    let raw = read_vec(device, SECURITY_SECTOR_OFFSET, 0x14)?;
    let serial: String = raw.iter().map(|&b| b as char).collect::<String>();
    let serial = serial.trim_matches(|c| c == '\0' || c == ' ').to_string();
    let count = read_vec(device, SECURITY_SECTOR_OFFSET + 0x58, 4)?;
    let sectors = u32::from_le_bytes([count[0], count[1], count[2], count[3]]);
    Ok(Some(SecuritySector {
        serial,
        claimed_length: sectors as u64 * 512,
    }))
}

/// Why a restore can't leave the target's own start in place, or None when it can. The
/// Content partition in a backup is laid out for the size the console saw back then, and the
/// console goes by the target's own security sector, so the two have to agree.
pub fn keep_problem(backup_visible_length: u64, target: Option<&SecuritySector>) -> Option<String> {
    let Some(target) = target else {
        return Some(
            "This drive has no security sector of its own, so there's nothing to keep. Leave this off and it gets the backup's."
                .into(),
        );
    };
    if target.claimed_length != backup_visible_length {
        return Some(format!(
            "The console sees this drive as {}, and the backup was made from a drive it saw as {}. \
             The Content partition in the backup is laid out for that size, so it wouldn't read properly here. \
             Keeping a drive's own security sector only works between drives of the same size.",
            size::format(target.claimed_length),
            size::format(backup_visible_length)
        ));
    }
    None
}

pub fn keep_problem_for(backup: &dyn BlockDevice, target: &dyn BlockDevice) -> Result<Option<String>> {
    Ok(keep_problem(layout::visible_length(backup)?, read(target)?.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemoryDevice;
    use crate::testing::console_drive;

    const GIB: u64 = 1 << 30;

    #[test]
    fn the_serial_and_size_are_read() {
        let drive = console_drive(6 * GIB, "WD-WXQ1A12B3456", 6 * GIB);
        let sector = read(&drive).unwrap().unwrap();
        assert_eq!(sector.serial, "WD-WXQ1A12B3456");
        assert_eq!(sector.claimed_length, 6 * GIB);
    }

    #[test]
    fn a_drive_without_one_has_none() {
        assert!(read(&MemoryDevice::new(6 * GIB)).unwrap().is_none());
        assert!(read(&MemoryDevice::new(4096)).unwrap().is_none());
        let junk = MemoryDevice::new(6 * GIB);
        junk.write_at(SECURITY_SECTOR_OFFSET, &[0x01; 0x20]).unwrap();
        assert!(read(&junk).unwrap().is_none());
    }

    #[test]
    fn the_part_a_drive_keeps_is_the_first_512_kb() {
        assert_eq!(HEADER_LENGTH, 0x80000);
    }

    #[test]
    fn drives_of_the_same_size_can_keep_their_own() {
        let backup = console_drive(6 * GIB, "AAAA", 6 * GIB);
        let target = console_drive(6 * GIB, "BBBB", 6 * GIB);
        assert_eq!(keep_problem_for(&backup, &target).unwrap(), None);
    }

    #[test]
    fn a_bigger_drive_cant_keep_its_own() {
        let backup = console_drive(6 * GIB, "AAAA", 6 * GIB);
        let target = console_drive(7 * GIB, "BBBB", 7 * GIB);
        let problem = keep_problem_for(&backup, &target).unwrap().unwrap();
        assert!(
            problem.contains("7 GB") && problem.contains("6 GB") && problem.contains("same size"),
            "{problem}"
        );
    }

    #[test]
    fn a_drive_with_no_security_sector_has_nothing_to_keep() {
        let backup = console_drive(6 * GIB, "AAAA", 6 * GIB);
        let target = MemoryDevice::new(6 * GIB);
        assert!(
            keep_problem_for(&backup, &target)
                .unwrap()
                .unwrap()
                .contains("nothing to keep")
        );
        assert!(keep_problem(6 * GIB, None).unwrap().contains("nothing to keep"));
    }

    #[test]
    fn the_size_a_drive_claims_is_what_counts() {
        // a disk the console is told is 6 GB although it's bigger
        let backup = console_drive(6 * GIB, "AAAA", 6 * GIB);
        let roomy = console_drive(8 * GIB, "BBBB", 6 * GIB);
        assert_eq!(keep_problem_for(&backup, &roomy).unwrap(), None);
    }
}
