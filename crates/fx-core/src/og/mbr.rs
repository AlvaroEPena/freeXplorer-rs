//! The partition table an original Xbox drive carries in its first sector.
//!
//! It is 432 bytes: a 16 byte magic, 32 reserved bytes, then twelve 32 byte entries. Every
//! field is little endian and measured in 512 byte sectors.
//!
//! Note that this is not the structure the console keeps in memory, which is 552 bytes of
//! 36 byte entries because it carries two fields the copy on disk has no room for. Tools have
//! written that one back to disk before, which runs over the boot loader at 0x200 — so the
//! size here is deliberately the small one.

use crate::device::BlockDevice;
use crate::error::{Error, Result};

/// Set on entries that describe a partition that exists.
pub const IN_USE: u32 = 0x8000_0000;

pub const SECTOR_SIZE: u64 = 512;
/// name[16], flags, start, size, and four bytes nothing uses
pub const ENTRY_SIZE: usize = 32;
pub const ENTRY_COUNT: usize = 12;
/// sixteen bytes of magic, then thirty two bytes nothing uses
pub const TABLE_PREFIX: usize = 48;
pub const TABLE_BYTES: usize = TABLE_PREFIX + ENTRY_COUNT * ENTRY_SIZE;

/// The first partition starts here, so nothing before it can be one.
pub const FIRST_PARTITION_LBA: u32 = 0x400;
/// A start or a length past this can't be addressed in 32 bit sectors.
pub const MAX_LBA: u64 = 0xFFFF_FFFF;

/// The magic XBPartitioner writes. Other writers use their own or none at all, so it is
/// reported rather than required.
pub const PARTINFO_MAGIC: &[u8; 16] = b"****PARTINFO****";

/// What a 360 drive's first sector starts with, so it can be told apart from this format.
pub const XBOX360_TABLE_MAGIC: u32 = 0x0002_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub index: usize,
    pub flags: u32,
    /// first sector of the partition
    pub start_lba: u32,
    /// size in sectors; zero on an entry that's just a name
    pub size_lba: u32,
    /// the name as stored, padded with spaces or zeroes
    pub name: [u8; 16],
}

impl Entry {
    /// A slot as a format writes it: the name padded with spaces, the in-use flag set.
    ///
    /// A name longer than sixteen bytes is cut, which is what the field is; every name a console
    /// drive carries fits, and `XBOX GAME SWAP 1` fills it exactly.
    pub fn new(name: &str, start_lba: u32, size_lba: u32) -> Entry {
        let mut padded = [0x20u8; 16];
        for (slot, byte) in padded.iter_mut().zip(name.bytes()) {
            *slot = byte;
        }
        Entry {
            index: 0,
            flags: IN_USE,
            start_lba,
            size_lba,
            name: padded,
        }
    }

    pub fn offset(&self) -> u64 {
        self.start_lba as u64 * SECTOR_SIZE
    }

    pub fn length(&self) -> u64 {
        self.size_lba as u64 * SECTOR_SIZE
    }

    pub fn end_lba(&self) -> u64 {
        self.start_lba as u64 + self.size_lba as u64
    }

    /// The stored name, trimmed. Only ever useful for showing to a person: two drives in the
    /// same layout can disagree about these, and one labels its data partition "XBOX SHELL".
    pub fn name(&self) -> String {
        let text: String = self
            .name
            .iter()
            .map(|b| if (0x20..0x7F).contains(b) { *b as char } else { ' ' })
            .collect();
        text.trim().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub magic: [u8; 16],
    /// entries that are in use and have a size, ordered by where they start
    pub entries: Vec<Entry>,
}

impl Table {
    pub fn looks_like_partinfo(&self) -> bool {
        &self.magic == PARTINFO_MAGIC
    }

    /// The partitions are only trusted if every one of them sits somewhere the console puts
    /// partitions: the five retail starts, or anywhere after the retail area for F and up.
    /// A table that fails this is treated as no table at all, and the caller falls back to
    /// the retail layout, which is the safe way to be wrong.
    pub fn parse(sector0: &[u8]) -> Option<Table> {
        if sector0.len() < TABLE_BYTES {
            return None;
        }
        // a 360 drive's first sector is its own partition table, which is not this one
        if u32::from_le_bytes(sector0[..4].try_into().unwrap()) == XBOX360_TABLE_MAGIC {
            return None;
        }
        let mut magic = [0u8; 16];
        magic.copy_from_slice(&sector0[..16]);

        let mut entries = Vec::new();
        for index in 0..ENTRY_COUNT {
            let at = TABLE_PREFIX + index * ENTRY_SIZE;
            let word = |offset: usize| u32::from_le_bytes(sector0[at + offset..at + offset + 4].try_into().unwrap());
            let flags = word(16);
            if flags & IN_USE == 0 {
                continue;
            }
            let start_lba = word(20);
            let size_lba = word(24);
            if size_lba == 0 || start_lba < FIRST_PARTITION_LBA {
                return None;
            }
            if start_lba as u64 + size_lba as u64 > MAX_LBA {
                return None;
            }
            let mut name = [0u8; 16];
            name.copy_from_slice(&sector0[at..at + 16]);
            entries.push(Entry {
                index,
                flags,
                start_lba,
                size_lba,
                name,
            });
        }

        if entries.is_empty() {
            return None;
        }
        entries.sort_by_key(|e| e.start_lba);
        if entries
            .windows(2)
            .any(|pair| pair[0].end_lba() > pair[1].start_lba as u64)
        {
            return None;
        }
        if !entries.iter().all(|e| super::layout::is_console_start(e.start_lba)) {
            return None;
        }
        if !entries.iter().any(|e| super::layout::is_essential_start(e.start_lba)) {
            return None;
        }
        Some(Table { magic, entries })
    }
}

/// The 432 bytes a table occupies, laid out the way a drive carries them.
///
/// An unused slot is sixteen spaces and then sixteen zeroes, which is what the drive measured here
/// has; the parser skips it because the in-use flag is among the zeroes. A used slot leaves its own
/// last four bytes zero, and so does the drive.
pub fn table_bytes(entries: &[Entry]) -> [u8; TABLE_BYTES] {
    let mut out = [0u8; TABLE_BYTES];
    out[..16].copy_from_slice(PARTINFO_MAGIC);
    for slot in 0..ENTRY_COUNT {
        let at = TABLE_PREFIX + slot * ENTRY_SIZE;
        out[at..at + 16].fill(0x20);
    }
    for (slot, entry) in entries.iter().take(ENTRY_COUNT).enumerate() {
        let at = TABLE_PREFIX + slot * ENTRY_SIZE;
        out[at..at + 16].copy_from_slice(&entry.name);
        out[at + 16..at + 20].copy_from_slice(&(entry.flags | IN_USE).to_le_bytes());
        out[at + 20..at + 24].copy_from_slice(&entry.start_lba.to_le_bytes());
        out[at + 24..at + 28].copy_from_slice(&entry.size_lba.to_le_bytes());
    }
    out
}

/// Writes a table into sector zero, and nothing else.
///
/// The rest of the sector is read back and written unchanged, so whatever a tool left past the
/// table survives, and the boot loader from `0x200` on is never in the write at all.
pub fn write(device: &dyn BlockDevice, entries: &[Entry]) -> Result<()> {
    if entries.len() > ENTRY_COUNT {
        return Err(Error::invalid(format!(
            "a table holds {ENTRY_COUNT} entries, not {}",
            entries.len()
        )));
    }
    if !device.can_write() {
        return Err(Error::invalid("the device is read-only"));
    }
    let mut sector = [0u8; SECTOR_SIZE as usize];
    if device.len() >= SECTOR_SIZE {
        device.read_at(0, &mut sector)?;
    }
    sector[..TABLE_BYTES].copy_from_slice(&table_bytes(entries));
    device.write_at(0, &sector)?;
    device.flush()?;
    Ok(())
}

/// Reads and parses sector zero, without caring whether there really is a table there.
pub fn read(device: &dyn BlockDevice) -> Result<Option<Table>> {
    if device.len() < SECTOR_SIZE {
        return Ok(None);
    }
    let mut sector0 = [0u8; SECTOR_SIZE as usize];
    device.read_at(0, &mut sector0)?;
    Ok(Table::parse(&sector0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Sector zero of a real 250 GB drive, formatted by HeXEn on a console running Cerbios:
    /// five retail partitions and one F that fills the rest of the disk.
    const SECTOR0_HEX: [&str; 11] = [
        "2a2a2a2a50415254494e464f2a2a2a2a0000000000000000000000000000000000000000000000000000000000000000",
        "58424f58205348454c4c2020202020200000008000f45500b09698000000000058424f58204441544120202020202020",
        "000000800054460000a00f000000000058424f582047414d452053574150203100000080000400000070170000000000",
        "58424f582047414d45205357415020320000008000741700007017000000000058424f582047414d4520535741502033",
        "0000008000e42e00007017000000000058424f5820462020202020202020202000000080b08aee00f2092c1c00000000",
        "58424f582047202020202020202020200000000000000000000000000000000020202020202020202020202020202020",
        "000000000000000000000000000000002020202020202020202020202020202000000000000000000000000000000000",
        "202020202020202020202020202020200000000000000000000000000000000020202020202020202020202020202020",
        "000000000000000000000000000000002020202020202020202020202020202000000000000000000000000000000000",
        "202020202020202020202020202020200000000000000000000000000000000020202020202020202020202020202020",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ];

    pub fn sector0() -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        for line in SECTOR0_HEX {
            for pair in line.as_bytes().chunks(2) {
                let text = std::str::from_utf8(pair).unwrap();
                out.push(u8::from_str_radix(text, 16).unwrap());
            }
        }
        assert_eq!(out.len(), 512);
        out
    }

    #[test]
    fn reads_the_table_off_a_real_drive() {
        let table = Table::parse(&sector0()).expect("the drive has a table");
        assert!(table.looks_like_partinfo());
        assert_eq!(table.entries.len(), 6);
    }

    #[test]
    fn entries_are_ordered_by_where_they_start() {
        let table = Table::parse(&sector0()).unwrap();
        let starts: Vec<u32> = table.entries.iter().map(|e| e.start_lba).collect();
        assert_eq!(starts, vec![0x400, 0x177400, 0x2EE400, 0x465400, 0x55F400, 0xEE8AB0]);
    }

    /// This drive calls its data partition "XBOX SHELL" and its system partition "XBOX DATA",
    /// and its extended partition "XBOX G" is a name with no size. Which is why partitions are
    /// identified by where they start and never by what they're called.
    #[test]
    fn the_names_are_not_reliable() {
        let table = Table::parse(&sector0()).unwrap();
        let name_at = |lba: u32| table.entries.iter().find(|e| e.start_lba == lba).unwrap().name();
        assert_eq!(name_at(0x55F400), "XBOX SHELL");
        assert_eq!(name_at(0x465400), "XBOX DATA");
        assert_eq!(name_at(0xEE8AB0), "XBOX F");
    }

    #[test]
    fn sizes_are_what_the_console_wrote() {
        let table = Table::parse(&sector0()).unwrap();
        let data = table.entries.iter().find(|e| e.start_lba == 0x55F400).unwrap();
        assert_eq!(data.length(), 5_120_024_576);
        let extended = table.entries.last().unwrap();
        assert_eq!(extended.start_lba, 0xEE8AB0);
        assert_eq!(extended.length(), 241_995_867_136);
    }

    #[test]
    fn the_boot_loader_in_sector_one_is_not_mistaken_for_entries() {
        // the bytes past 0x200 are x86, and they were never parsed as a table
        let sector0 = sector0();
        let table = Table::parse(&sector0).unwrap();
        assert!(table.entries.iter().all(|e| e.index < 7));
    }

    #[test]
    fn a_zeroed_sector_is_not_a_table() {
        assert!(Table::parse(&[0u8; 512]).is_none());
    }

    #[test]
    fn a_360_partition_table_is_not_this_one() {
        let mut sector0 = sector0();
        sector0[..4].copy_from_slice(&XBOX360_TABLE_MAGIC.to_le_bytes());
        assert!(Table::parse(&sector0).is_none());
    }

    #[test]
    fn a_table_without_the_system_or_data_partition_is_not_believed() {
        // slots 0 and 1 are this drive's E and C
        let mut sector0 = sector0();
        for slot in [0, 1] {
            let entry = TABLE_PREFIX + slot * ENTRY_SIZE;
            sector0[entry + 16..entry + 20].copy_from_slice(&0u32.to_le_bytes());
        }
        assert!(Table::parse(&sector0).is_none());
    }

    #[test]
    fn a_short_read_is_not_a_table() {
        assert!(Table::parse(&sector0()[..256]).is_none());
    }

    #[test]
    fn a_partition_that_overlaps_another_is_refused() {
        let mut sector0 = sector0();
        // stretch Z over C
        let entry = TABLE_PREFIX + 4 * ENTRY_SIZE;
        sector0[entry + 24..entry + 28].copy_from_slice(&0x200000u32.to_le_bytes());
        assert!(Table::parse(&sector0).is_none());
    }

    #[test]
    fn a_partition_out_of_the_console_s_way_is_refused() {
        let mut sector0 = sector0();
        // move Y somewhere the console would never put it
        let entry = TABLE_PREFIX + 3 * ENTRY_SIZE;
        sector0[entry + 20..entry + 24].copy_from_slice(&0x123456u32.to_le_bytes());
        assert!(Table::parse(&sector0).is_none());
    }
}
