//! Working out whether a drive is an original Xbox drive.
//!
//! Two things can make it one: a partition table in the first sector, or FATX volumes sitting
//! at the retail offsets. The first is how a drive bigger than the retail BIOS understood gets
//! its space back; the second is a drive nobody ever gave a table to.

use crate::device::BlockDevice;
use crate::error::Result;
use crate::partition::PartitionSpec;
use crate::scan::{DetectedPartition, DriveKind, DriveScan};

use super::{layout, mbr};

pub fn scan(device: &dyn BlockDevice) -> Result<Option<DriveScan>> {
    let table = mbr::read(device)?;
    let (specs, has_table): (Vec<PartitionSpec>, bool) = match &table {
        Some(table) => (layout::from_table(table, device.len()), true),
        None => {
            // A quick format writes a table and a boot sector, not the whole disk, so a drive a PC
            // has just formatted still carries the FATX headers it had before. Those headers are
            // not evidence that the drive is an Xbox drive any more.
            if pc_formatted(device)? {
                return Ok(None);
            }
            (layout::retail(device.len()), false)
        }
    };

    let mut partitions = Vec::with_capacity(specs.len());
    let mut any_fatx = false;
    for spec in specs {
        if spec.length == 0 || spec.offset >= device.len() {
            partitions.push(DetectedPartition {
                spec,
                has_signature: false,
                flavor: None,
            });
            continue;
        }
        let flavor = crate::scan::signature_at(device, spec.offset)?;
        any_fatx |= flavor.is_some();
        partitions.push(DetectedPartition {
            spec,
            has_signature: flavor.is_some(),
            flavor,
        });
    }

    // A table is evidence on its own: something wrote a console partition table here. The
    // retail layout is guesswork, so it only counts if a FATX volume is really where it says.
    if !has_table && !any_fatx {
        return Ok(None);
    }

    Ok(Some(DriveScan {
        kind: DriveKind::OgDrive,
        visible_length: device.len() / mbr::SECTOR_SIZE * mbr::SECTOR_SIZE,
        has_security_sector: false,
        partitions,
    }))
}

/// The partition types a PC gives a partition.
///
/// An original Xbox drive's first sector is a boot sector with a table in it, never a PC's
/// partition table, so one of these at a start a PC would use is a PC's work.
const PC_PARTITION_TYPES: [u8; 13] = [
    0x01, 0x04, 0x06, 0x07, 0x0B, 0x0C, 0x0E, 0x0F, 0x27, 0x83, 0x8E, 0xAF, 0xEF,
];

/// Whether a PC, rather than a console, wrote the start of this drive.
///
/// The retail layout is guesswork, and it is the right guess for a drive nobody ever gave a table
/// to. It is the wrong guess for a drive a PC has formatted: every PC puts its first partition on a
/// 1 MiB boundary, or at 63 on an older disk, and leaves a partition table behind. The console's
/// own boot sector also carries the `0x55AA` signature, so the signature alone decides nothing.
fn pc_formatted(device: &dyn BlockDevice) -> Result<bool> {
    if device.len() < mbr::SECTOR_SIZE {
        return Ok(false);
    }
    let mut sector0 = [0u8; mbr::SECTOR_SIZE as usize];
    device.read_at(0, &mut sector0)?;

    // a GPT drive keeps its header in the next sector and a protective entry in this one
    if device.len() >= 2 * mbr::SECTOR_SIZE {
        let mut header = [0u8; 8];
        device.read_at(mbr::SECTOR_SIZE, &mut header)?;
        if &header == b"EFI PART" {
            return Ok(true);
        }
    }
    if &sector0[510..512] != b"\x55\xaa" {
        return Ok(false);
    }

    let sectors = device.len() / mbr::SECTOR_SIZE;
    for slot in 0..4 {
        let at = 446 + slot * 16;
        let kind = sector0[at + 4];
        if kind == 0xEE {
            return Ok(true);
        }
        let start = u32::from_le_bytes(sector0[at + 8..at + 12].try_into().unwrap());
        let count = u32::from_le_bytes(sector0[at + 12..at + 16].try_into().unwrap());
        if count == 0 || start == 0 || u64::from(start) >= sectors {
            continue;
        }
        if !PC_PARTITION_TYPES.contains(&kind) {
            continue;
        }
        if start % 2048 == 0 || start == 63 || start == 64 {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemoryDevice;

    /// The real table, on a drive of the length it came off.
    fn drive_with_table() -> MemoryDevice {
        let device = MemoryDevice::new(250_000_000_000);
        device.write_at(0, &crate::og::mbr::tests::sector0()).unwrap();
        device
    }

    fn write_signature(device: &MemoryDevice, at: u64) {
        device.write_at(at, b"FATX").unwrap();
    }

    #[test]
    fn a_table_is_enough_on_its_own() {
        let scan = scan(&drive_with_table()).unwrap().expect("a drive with a table");
        assert_eq!(scan.kind, DriveKind::OgDrive);
        assert_eq!(scan.partitions.len(), 6);
        assert!(!scan.has_security_sector);
    }

    #[test]
    fn the_partitions_the_table_lists_are_all_there() {
        let scan = scan(&drive_with_table()).unwrap().unwrap();
        let ids: Vec<&str> = scan.partitions.iter().map(|p| p.id()).collect();
        assert_eq!(ids, ["x", "y", "z", "c", "e", "f"]);
    }

    #[test]
    fn a_fatx_volume_somewhere_the_retail_layout_expects_is_an_xbox_drive() {
        let device = MemoryDevice::new(250_000_000_000);
        device.write_at(0, &[0u8; 512]).unwrap();
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        write_signature(&device, c.offset);

        let scan = scan(&device).unwrap().expect("it's an original Xbox drive");
        assert_eq!(scan.kind, DriveKind::OgDrive);
        let c = scan.partitions.iter().find(|p| p.id() == "c").unwrap();
        assert!(c.can_mount());
        assert_eq!(c.flavor, Some(crate::fatx::Flavor::Xbox));
    }

    #[test]
    fn a_blank_drive_is_not_an_xbox_drive() {
        let device = MemoryDevice::new(250_000_000_000);
        assert!(scan(&device).unwrap().is_none());
    }

    #[test]
    fn a_volume_without_a_fatx_signature_doesnt_count() {
        let device = MemoryDevice::new(250_000_000_000);
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        device.write_at(c.offset, b"NOPE").unwrap();
        assert!(scan(&device).unwrap().is_none());
    }

    /// A partition table as a PC writes it: the signature, and one entry.
    fn pc_mbr(start_lba: u32, kind: u8, count: u32) -> [u8; 512] {
        let mut sector0 = [0u8; 512];
        sector0[510..512].copy_from_slice(b"\x55\xaa");
        sector0[446 + 4] = kind;
        sector0[446 + 8..446 + 12].copy_from_slice(&start_lba.to_le_bytes());
        sector0[446 + 12..446 + 16].copy_from_slice(&count.to_le_bytes());
        sector0
    }

    /// The drive that prompted this: Windows formatted it as exFAT, and a quick format left every
    /// FATX header where it was. The PC's table is the newer statement about the drive.
    #[test]
    fn a_pc_partition_table_is_not_an_xbox_drive() {
        let device = MemoryDevice::new(250_000_000_000);
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        write_signature(&device, c.offset);

        device.write_at(0, &pc_mbr(2048, 0x07, 488_000_000)).unwrap();
        assert!(scan(&device).unwrap().is_none());
    }

    #[test]
    fn a_gpt_drive_is_not_an_xbox_drive() {
        let device = MemoryDevice::new(250_000_000_000);
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        write_signature(&device, c.offset);

        device.write_at(0, &pc_mbr(1, 0xEE, 0xFFFF_FFFF)).unwrap();
        device.write_at(512, b"EFI PART").unwrap();
        assert!(scan(&device).unwrap().is_none());
    }

    /// A drive nobody gave a table to still has a boot sector, and that sector ends in the same
    /// `0x55AA` a PC's table does. Only a real PC entry makes the drive a PC's.
    #[test]
    fn a_boot_sector_without_pc_entries_leaves_the_retail_layout_alone() {
        let device = MemoryDevice::new(250_000_000_000);
        let mut sector0 = [0u8; 512];
        sector0[510..512].copy_from_slice(b"\x55\xaa");
        device.write_at(0, &sector0).unwrap();
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        write_signature(&device, c.offset);

        assert!(scan(&device).unwrap().is_some());
    }

    /// Boot code is not a partition table: a type byte and a start that no PC would choose is
    /// left alone, so a real drive is never called a PC drive by accident.
    #[test]
    fn a_pc_entry_only_counts_at_a_start_a_pc_would_use() {
        let device = MemoryDevice::new(250_000_000_000);
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        write_signature(&device, c.offset);

        device.write_at(0, &pc_mbr(0x123456, 0x07, 1000)).unwrap();
        assert!(scan(&device).unwrap().is_some());
    }

    /// A 360 volume sitting where the retail layout expects C is still an original Xbox drive:
    /// the drive is laid out like one, only its file system was written by the other console.
    #[test]
    fn the_layout_decides_the_console_not_the_volume_flavor() {
        let device = MemoryDevice::new(250_000_000_000);
        let specs = layout::retail(device.len());
        let c = layout::find(&specs, "c").unwrap();
        device.write_at(c.offset, b"XTAF").unwrap();

        let scan = scan(&device).unwrap().expect("the layout says original Xbox");
        let c = scan.partitions.iter().find(|p| p.id() == "c").unwrap();
        assert_eq!(c.flavor, Some(crate::fatx::Flavor::Xbox360));
    }
}
