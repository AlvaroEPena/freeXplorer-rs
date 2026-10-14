use std::num::NonZeroUsize;

use lru::LruCache;

use super::geometry::Geometry;
use crate::device::Device;
use crate::error::{Error, ErrorKind, Result};

const PAGE_SIZE: usize = 4096;
const READ_AHEAD: u64 = 16;
const MAX_RUN: u64 = 256;

struct Page {
    data: Box<[u8]>,
    dirty: bool,
}

/// The allocation table, read in 4K pages as needed. On a big drive the FAT is hundreds of
/// MB so it can't just be loaded. Writes stay in memory until `flush`.
pub(crate) struct FatTable {
    device: Device,
    start: u64,
    entry_size: usize,
    big_endian: bool,
    page_count: u64,
    cache: LruCache<u64, Page>,
    capacity: u64,
}

impl FatTable {
    pub fn new(device: Device, geometry: &Geometry, big_endian: bool) -> Self {
        Self::with_cache(device, geometry, big_endian, 16384)
    }

    pub fn with_cache(device: Device, geometry: &Geometry, big_endian: bool, max_pages: usize) -> Self {
        let entry_size = geometry.fat_entry_size as usize;
        FatTable {
            device,
            start: geometry.fat_offset(),
            entry_size,
            big_endian,
            page_count: geometry.fat_size / PAGE_SIZE as u64,
            cache: LruCache::new(NonZeroUsize::new(max_pages.max(2)).unwrap()),
            capacity: geometry.fat_size / entry_size as u64,
        }
    }

    #[cfg(test)]
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn get(&mut self, index: u32) -> Result<u32> {
        let at = index as u64 * self.entry_size as u64;
        let (entry_size, big) = (self.entry_size, self.big_endian);
        let page = self.page(at / PAGE_SIZE as u64)?;
        let within = (at % PAGE_SIZE as u64) as usize;
        let bytes = &page.data[within..within + entry_size];
        Ok(match (entry_size, big) {
            (2, true) => u16::from_be_bytes([bytes[0], bytes[1]]) as u32,
            (2, false) => u16::from_le_bytes([bytes[0], bytes[1]]) as u32,
            (_, true) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            (_, false) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        })
    }

    pub fn set(&mut self, index: u32, value: u32) -> Result<()> {
        let at = index as u64 * self.entry_size as u64;
        let (entry_size, big) = (self.entry_size, self.big_endian);
        let page = self.page(at / PAGE_SIZE as u64)?;
        let within = (at % PAGE_SIZE as u64) as usize;
        let target = &mut page.data[within..within + entry_size];
        match (entry_size, big) {
            (2, true) => target.copy_from_slice(&(value as u16).to_be_bytes()),
            (2, false) => target.copy_from_slice(&(value as u16).to_le_bytes()),
            (_, true) => target.copy_from_slice(&value.to_be_bytes()),
            (_, false) => target.copy_from_slice(&value.to_le_bytes()),
        }
        page.dirty = true;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let mut dirty: Vec<u64> = self.cache.iter().filter(|(_, p)| p.dirty).map(|(n, _)| *n).collect();
        if dirty.is_empty() {
            return Ok(());
        }
        dirty.sort_unstable();

        // neighbouring pages go out in one write
        let mut i = 0;
        while i < dirty.len() {
            let mut j = i;
            while j + 1 < dirty.len() && dirty[j + 1] == dirty[j] + 1 && ((j - i) as u64) < MAX_RUN - 1 {
                j += 1;
            }
            let mut buffer = Vec::with_capacity((j - i + 1) * PAGE_SIZE);
            for number in &dirty[i..=j] {
                buffer.extend_from_slice(&self.cache.peek(number).unwrap().data);
            }
            self.device
                .write_at(self.start + dirty[i] * PAGE_SIZE as u64, &buffer)?;
            for number in &dirty[i..=j] {
                self.cache.peek_mut(number).unwrap().dirty = false;
            }
            i = j + 1;
        }
        Ok(())
    }

    /// Counts the entries that are zero for clusters 1..=last, reading the table straight
    /// off the device in big chunks.
    pub fn count_free(&mut self, last: u32, mut progress: impl FnMut(f64)) -> Result<u64> {
        self.flush()?;
        const CHUNK: usize = 1 << 20;
        let entry = self.entry_size;
        let total = ((last as u64 + 1).min(self.capacity)) * entry as u64;
        let mut free = 0u64;
        let mut buffer = vec![0u8; CHUNK];
        let mut pos = 0u64;
        while pos < total {
            let n = (CHUNK as u64).min(total - pos) as usize;
            self.device.read_at(self.start + pos, &mut buffer[..n])?;
            // entry 0 is the media byte, not a cluster
            let skip = if pos == 0 { entry } else { 0 };
            free += buffer[skip..n / entry * entry]
                .chunks_exact(entry)
                .filter(|e| e.iter().all(|&b| b == 0))
                .count() as u64;
            pos += n as u64;
            progress(pos as f64 / total as f64);
        }
        Ok(free)
    }

    fn page(&mut self, number: u64) -> Result<&mut Page> {
        if !self.cache.contains(&number) {
            self.load(number)?;
        }
        Ok(self.cache.get_mut(&number).unwrap())
    }

    fn load(&mut self, number: u64) -> Result<()> {
        if number >= self.page_count {
            return Err(Error::fatx(
                ErrorKind::Corrupt,
                format!("FAT page {number} is outside the table"),
            ));
        }
        let first = number - number % READ_AHEAD;
        let count = READ_AHEAD.min(self.page_count - first) as usize;
        let mut raw = vec![0u8; count * PAGE_SIZE];
        self.device.read_at(self.start + first * PAGE_SIZE as u64, &mut raw)?;

        // the wanted page goes in last so it's the newest. A page that's already cached
        // may hold edits that aren't on disk yet, so those are left alone.
        let order = (0..count as u64)
            .filter(|i| first + i != number)
            .chain(Some(number - first));
        for i in order {
            let n = first + i;
            if n != number && self.cache.contains(&n) {
                continue;
            }
            let from = i as usize * PAGE_SIZE;
            let page = Page {
                data: raw[from..from + PAGE_SIZE].to_vec().into_boxed_slice(),
                dirty: false,
            };
            if let Some((evicted, old)) = self.cache.push(n, page)
                && evicted != n
                && old.dirty
            {
                self.device
                    .write_at(self.start + evicted * PAGE_SIZE as u64, &old.data)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::device::MemoryDevice;

    fn table(max_pages: usize) -> (FatTable, Device) {
        table_for(512 << 20, max_pages)
    }

    fn table_for(length: u64, max_pages: usize) -> (FatTable, Device) {
        let g = Geometry::compute(length, 0x20).unwrap();
        let device: Device = Arc::new(MemoryDevice::new(length));
        (FatTable::with_cache(device.clone(), &g, true, max_pages), device)
    }

    #[test]
    fn what_was_set_reads_back() {
        let (mut fat, _) = table(64);
        for i in [1u32, 2, 100, 5000, 20000] {
            fat.set(i, i + 7).unwrap();
        }
        for i in [1u32, 2, 100, 5000, 20000] {
            assert_eq!(fat.get(i).unwrap(), i + 7);
        }
    }

    #[test]
    fn entries_are_stored_big_endian_when_asked() {
        let (mut fat, device) = table(64);
        fat.set(3, 0x1234).unwrap();
        fat.flush().unwrap();
        let g = Geometry::compute(512 << 20, 0x20).unwrap();
        let mut raw = [0u8; 2];
        device.read_at(g.fat_offset() + 6, &mut raw).unwrap();
        assert_eq!(raw, [0x12, 0x34]);
    }

    #[test]
    fn a_tiny_cache_still_keeps_every_edit() {
        // 4 GiB has a 32 bit FAT of 256 pages, a thousand entries each
        let (mut fat, _) = table_for(4 << 30, 4);
        // spread the edits over far more pages than the cache holds
        for i in 0..40u32 {
            fat.set(i * 3000 + 1, i + 1).unwrap();
        }
        for i in 0..40u32 {
            assert_eq!(fat.get(i * 3000 + 1).unwrap(), i + 1, "entry {i}");
        }
        fat.flush().unwrap();
        for i in 0..40u32 {
            assert_eq!(fat.get(i * 3000 + 1).unwrap(), i + 1);
        }
    }

    #[test]
    fn flushed_edits_survive_a_new_table_over_the_same_device() {
        let (mut fat, device) = table(64);
        fat.set(10, 11).unwrap();
        fat.flush().unwrap();
        let g = Geometry::compute(512 << 20, 0x20).unwrap();
        let mut again = FatTable::new(device, &g, true);
        assert_eq!(again.get(10).unwrap(), 11);
    }

    #[test]
    fn counting_free_entries_skips_the_media_entry() {
        let (mut fat, _) = table(64);
        fat.set(0, 0xFFF8).unwrap();
        fat.set(1, 0xFFFF).unwrap();
        fat.set(2, 3).unwrap();
        let free = fat.count_free(9, |_| {}).unwrap();
        assert_eq!(free, 7); // clusters 3..=9
    }

    #[test]
    fn reading_outside_the_table_is_an_error() {
        let (mut fat, _) = table(64);
        let past = (fat.capacity() as u32) + 10;
        assert!(fat.get(past).is_err());
    }
}
