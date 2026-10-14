use crate::device::{BlockDevice, read_vec};
use crate::error::Result;

pub const SECURITY_SECTOR_OFFSET: u64 = 0x2000;
pub const SYSTEM_CACHE_OFFSET: u64 = 0x80000;
pub const SYSTEM_CACHE_LENGTH: u64 = 0x8000_0000;
pub const GAME_CACHE_OFFSET: u64 = 0x8008_0000;
pub const GAME_CACHE_LENGTH: u64 = 0xA0E3_0000;
pub const SYSTEM_EXT_OFFSET: u64 = 0x1_0C08_0000;
pub const SYSTEM_EXT_LENGTH: u64 = 0xCE3_0000;
pub const SYSTEM_EXT2_OFFSET: u64 = 0x1_18EB_0000;
pub const SYSTEM_EXT2_LENGTH: u64 = 0x800_0000;
pub const COMPAT_OFFSET: u64 = 0x1_20EB_0000;
pub const COMPAT_LENGTH: u64 = 0x1000_0000;
pub const CONTENT_OFFSET: u64 = 0x1_30EB_0000;

/// smallest drive that has any room for a Content partition
pub const MINIMUM_DRIVE_LENGTH: u64 = CONTENT_OFFSET + 0x1000_0000;

pub const SYSTEM_CACHE_ID: &str = "syscache";
pub const GAME_CACHE_ID: &str = "gamecache";
pub const SYSTEM_EXT_ID: &str = "sysext";
pub const SYSTEM_EXT2_ID: &str = "sysext2";
pub const COMPAT_ID: &str = "compat";
pub const CONTENT_ID: &str = "content";

/// the game cache region as the public docs list it also covers the two sysext partitions,
/// so the part that's really cache stops where SysExt starts
const SYSTEM_EXT_REGION_LENGTH: u64 = SYSTEM_EXT_LENGTH + SYSTEM_EXT2_LENGTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionFileSystem {
    Fatx,
    Sfcx,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub offset: u64,
    pub length: u64,
    pub file_system: PartitionFileSystem,
    pub description: &'static str,
}

impl PartitionSpec {
    pub fn is_fatx(&self) -> bool {
        self.file_system == PartitionFileSystem::Fatx
    }

    pub fn end(&self) -> u64 {
        self.offset + self.length
    }
}

/// The console only sees as many sectors as the security sector claims, and that
/// count is a 32 bit number, so nothing past 2 TiB exists as far as it's concerned.
pub fn visible_length(device: &dyn BlockDevice) -> Result<u64> {
    let length = device.len() / 512 * 512;
    let mut visible = length.min(0xFFFF_FFFFu64 * 512);

    if length >= SECURITY_SECTOR_OFFSET + 0x5C {
        let mut raw = [0u8; 4];
        device.read_at(SECURITY_SECTOR_OFFSET + 0x58, &mut raw)?;
        let claimed = u32::from_le_bytes(raw) as u64 * 512;
        if claimed > CONTENT_OFFSET + COMPAT_LENGTH && claimed <= length {
            visible = claimed;
        }
    }
    Ok(visible)
}

/// A serial number's worth of printable text where the security sector starts.
pub fn looks_like_security_sector(device: &dyn BlockDevice) -> Result<bool> {
    if device.len() < SECURITY_SECTOR_OFFSET + 0x20 {
        return Ok(false);
    }
    let serial = read_vec(device, SECURITY_SECTOR_OFFSET, 0x14)?;
    let mut any_text = false;
    for b in serial {
        if b == 0 || b == 0x20 {
            continue;
        }
        if !(0x21..=0x7E).contains(&b) {
            return Ok(false);
        }
        any_text = true;
    }
    Ok(any_text)
}

/// Every partition the console knows about, with Content stretched to the end of what's visible.
pub fn partitions(visible_length: u64) -> Vec<PartitionSpec> {
    let content_length = visible_length.saturating_sub(CONTENT_OFFSET);
    vec![
        PartitionSpec {
            id: SYSTEM_CACHE_ID,
            name: "System Cache",
            offset: SYSTEM_CACHE_OFFSET,
            length: SYSTEM_CACHE_LENGTH,
            file_system: PartitionFileSystem::Sfcx,
            description: "Console cache. Not FATX, can't be browsed.",
        },
        PartitionSpec {
            id: GAME_CACHE_ID,
            name: "Game Cache",
            offset: GAME_CACHE_OFFSET,
            length: GAME_CACHE_LENGTH - SYSTEM_EXT_REGION_LENGTH,
            file_system: PartitionFileSystem::Sfcx,
            description: "Console cache. Not FATX, can't be browsed.",
        },
        PartitionSpec {
            id: SYSTEM_EXT_ID,
            name: "System Extended",
            offset: SYSTEM_EXT_OFFSET,
            length: SYSTEM_EXT_LENGTH,
            file_system: PartitionFileSystem::Fatx,
            description: "Kinect and Avatar system files.",
        },
        PartitionSpec {
            id: SYSTEM_EXT2_ID,
            name: "System Extended 2",
            offset: SYSTEM_EXT2_OFFSET,
            length: SYSTEM_EXT2_LENGTH,
            file_system: PartitionFileSystem::Fatx,
            description: "Second system extended partition.",
        },
        PartitionSpec {
            id: COMPAT_ID,
            name: "Xbox 1 Compatibility",
            offset: COMPAT_OFFSET,
            length: COMPAT_LENGTH,
            file_system: PartitionFileSystem::Fatx,
            description: "Original Xbox backwards compatibility.",
        },
        PartitionSpec {
            id: CONTENT_ID,
            name: "Content",
            offset: CONTENT_OFFSET,
            length: content_length,
            file_system: PartitionFileSystem::Fatx,
            description: "Games, DLC, saves, profiles.",
        },
    ]
}

pub fn find(id_or_alias: &str, visible_length: u64) -> Option<PartitionSpec> {
    let id = normalize(id_or_alias);
    partitions(visible_length).into_iter().find(|p| p.id == id)
}

/// Turns the ways people spell a partition into its id.
pub fn normalize(id_or_alias: &str) -> String {
    let s: String = id_or_alias
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect();
    match s.as_str() {
        "content" | "data" | "hdd1" | "partition3" => CONTENT_ID.into(),
        "compat" | "compatibility" | "xbox1" | "backcompat" | "backwardscompatibility" | "hddx" | "partition2" => {
            COMPAT_ID.into()
        }
        "sysext" | "systemextended" => SYSTEM_EXT_ID.into(),
        "sysext2" | "systemextended2" => SYSTEM_EXT2_ID.into(),
        "syscache" | "systemcache" | "cache0" => SYSTEM_CACHE_ID.into(),
        "gamecache" | "cache1" => GAME_CACHE_ID.into(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MemoryDevice;

    fn sized(length: u64) -> MemoryDevice {
        MemoryDevice::new(length)
    }

    #[test]
    fn the_offsets_are_the_ones_a_real_drive_has() {
        let all = partitions(8 << 30);
        let at = |id: &str| all.iter().find(|p| p.id == id).unwrap().clone();
        assert_eq!(at("sysext").offset, 0x10C080000);
        assert_eq!(at("sysext2").offset, 0x118EB0000);
        assert_eq!(at("compat").offset, 0x120EB0000);
        assert_eq!(at("content").offset, 0x130EB0000);
        assert_eq!(at("content").length, (8u64 << 30) - 0x130EB0000);
        assert_eq!(at("gamecache").length, 0x8C00_0000);
    }

    #[test]
    fn partitions_dont_overlap() {
        let all = partitions(120 << 30);
        for pair in all.windows(2) {
            assert!(
                pair[0].end() <= pair[1].offset,
                "{} runs into {}",
                pair[0].name,
                pair[1].name
            );
        }
    }

    #[test]
    fn aliases_find_the_same_partition() {
        for alias in ["Content", "DATA", "hdd1", "partition 3"] {
            assert_eq!(normalize(alias), "content", "{alias}");
        }
        for alias in ["Compatibility", "xbox-1", "back_compat"] {
            assert_eq!(normalize(alias), "compat", "{alias}");
        }
        assert_eq!(normalize("System Extended 2"), "sysext2");
        assert_eq!(normalize("nonsense"), "nonsense");
    }

    #[test]
    fn a_drive_without_a_claim_is_as_big_as_the_disk() {
        let device = sized(8 << 30);
        assert_eq!(visible_length(&device).unwrap(), 8 << 30);
    }

    #[test]
    fn the_claimed_size_wins_when_it_is_believable() {
        let device = sized(10 << 30);
        device
            .write_at(SECURITY_SECTOR_OFFSET + 0x58, &((8u64 << 30) / 512).to_le_bytes()[..4])
            .unwrap();
        assert_eq!(visible_length(&device).unwrap(), 8 << 30);
    }

    #[test]
    fn a_claim_bigger_than_the_disk_or_absurdly_small_is_ignored() {
        let device = sized(8 << 30);
        device
            .write_at(SECURITY_SECTOR_OFFSET + 0x58, &((16u64 << 30) / 512).to_le_bytes()[..4])
            .unwrap();
        assert_eq!(visible_length(&device).unwrap(), 8 << 30);
        device
            .write_at(SECURITY_SECTOR_OFFSET + 0x58, &1000u32.to_le_bytes())
            .unwrap();
        assert_eq!(visible_length(&device).unwrap(), 8 << 30);
    }

    #[test]
    fn nothing_past_2_tib_is_visible() {
        let device = sized(4 << 40);
        assert_eq!(visible_length(&device).unwrap(), 0xFFFF_FFFFu64 * 512);
    }

    #[test]
    fn a_serial_number_makes_a_security_sector_look_real() {
        let device = sized(8 << 30);
        assert!(!looks_like_security_sector(&device).unwrap());
        device
            .write_at(SECURITY_SECTOR_OFFSET, b"WD-WXQ1A12B3456     ")
            .unwrap();
        assert!(looks_like_security_sector(&device).unwrap());
        device.write_at(SECURITY_SECTOR_OFFSET, &[1, 2, 3, 4]).unwrap();
        assert!(!looks_like_security_sector(&device).unwrap());
    }
}
