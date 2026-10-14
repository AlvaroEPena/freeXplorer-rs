use crate::error::{Error, ErrorKind, Result};

pub const SECTOR_SIZE: u64 = 512;
pub const HEADER_SIZE: u64 = 0x1000;
pub const MAX_FILE_NAME_LENGTH: usize = 42;
pub const MAX_DATA_CLUSTERS: u64 = 0x1000_0000;

/// 16 bit entries below this many clusters, 32 bit from here up
const FAT16_LIMIT: u64 = 0xFFF0;

/// Where everything lives inside a partition. The console works all of this out from the
/// partition size alone, there's nothing on disk that says how big the FAT is, so the
/// maths here has to match it exactly or every cluster ends up in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    pub partition_length: u64,
    pub sectors_per_cluster: u32,
    pub cluster_size: u32,
    pub fat_entry_size: u32,
    pub fat_size: u64,
    pub data_offset: u64,
    pub cluster_count: u32,
}

impl Geometry {
    pub fn is_valid_sectors_per_cluster(spc: u32) -> bool {
        (1..=128).contains(&spc) && spc.is_power_of_two()
    }

    pub fn compute(partition_length: u64, sectors_per_cluster: u32) -> Result<Geometry> {
        if !Self::is_valid_sectors_per_cluster(sectors_per_cluster) {
            return Err(Error::fatx(
                ErrorKind::NotFatx,
                format!("{sectors_per_cluster} sectors per cluster doesn't make sense"),
            ));
        }
        let spc = sectors_per_cluster as u64;
        let sectors = partition_length / SECTOR_SIZE;
        let max_clusters = sectors / spc + 1;
        let entry: u32 = if max_clusters < FAT16_LIMIT { 2 } else { 4 };
        let fat_size = round_up(max_clusters * entry as u64, HEADER_SIZE);
        let data_offset = HEADER_SIZE + fat_size;
        let total = sectors * SECTOR_SIZE;
        // a partition too small for its own FAT leaves nothing usable
        let usable = total.saturating_sub(data_offset) / (spc * SECTOR_SIZE);
        if usable < 2 {
            return Err(Error::fatx(
                ErrorKind::NotFatx,
                "partition is too small to hold a FATX volume",
            ));
        }
        Ok(Geometry {
            partition_length: total,
            sectors_per_cluster,
            cluster_size: sectors_per_cluster * SECTOR_SIZE as u32,
            fat_entry_size: entry,
            fat_size,
            data_offset,
            cluster_count: usable.min(u32::MAX as u64 - 16) as u32,
        })
    }

    /// Smallest standard cluster size (16K and up) that keeps the volume under the console's
    /// cluster limit. Anything the 360 formats itself uses 16K, bigger drives need more.
    pub fn pick_sectors_per_cluster(partition_length: u64) -> u32 {
        let mut spc = 0x20u32;
        while spc <= 0x80 {
            let sectors = partition_length / SECTOR_SIZE;
            let max_clusters = sectors / spc as u64 + 1;
            if max_clusters - 1 <= MAX_DATA_CLUSTERS {
                return spc;
            }
            spc *= 2;
        }
        0x80
    }

    pub fn fat_offset(&self) -> u64 {
        HEADER_SIZE
    }

    pub fn is_fat32(&self) -> bool {
        self.fat_entry_size == 4
    }

    pub fn end_of_chain(&self) -> u32 {
        if self.is_fat32() { 0xFFFF_FFFF } else { 0xFFFF }
    }

    pub fn media_entry(&self) -> u32 {
        if self.is_fat32() { 0xFFFF_FFF8 } else { 0xFFF8 }
    }

    pub fn bad_cluster(&self) -> u32 {
        if self.is_fat32() { 0xFFFF_FFF7 } else { 0xFFF7 }
    }

    /// anything at or above this ends a chain
    pub fn end_threshold(&self) -> u32 {
        if self.is_fat32() { 0xFFFF_FFF8 } else { 0xFFF8 }
    }

    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        self.data_offset + (cluster as u64 - 1) * self.cluster_size as u64
    }
}

fn round_up(value: u64, multiple: u64) -> u64 {
    value.div_ceil(multiple) * multiple
}

#[cfg(test)]
mod tests {
    use super::*;

    // numbers taken from the example output of a public XTAF reader run on a real drive:
    // 16K clusters, 4 byte FAT entries, FAT of 59801600 bytes
    #[test]
    fn matches_a_real_data_partition() {
        let g = Geometry::compute(244_943_675_392, 0x20).unwrap();
        assert_eq!(g.cluster_size, 16384);
        assert_eq!(g.fat_entry_size, 4);
        assert_eq!(g.fat_size, 59_801_600);
        assert_eq!(g.data_offset, 0x1000 + 59_801_600);
    }

    // same source: a 128 MiB sysext partition has its first cluster at 0x6000
    #[test]
    fn matches_a_real_sysext_partition() {
        let g = Geometry::compute(0x800_0000, 0x20).unwrap();
        assert_eq!(g.fat_entry_size, 2);
        assert_eq!(g.fat_size, 0x5000);
        assert_eq!(g.data_offset, 0x6000);
        assert_eq!(g.cluster_offset(1), 0x6000);
        assert_eq!(g.cluster_offset(2), 0xA000);
    }

    #[test]
    fn switches_to_32_bit_entries_at_0xfff0_clusters() {
        // max clusters = sectors / spc + 1
        let just_below = (0xFFEF - 1) * 0x20 * 512;
        let at_limit = (0xFFF0 - 1) * 0x20 * 512;
        assert_eq!(Geometry::compute(just_below, 0x20).unwrap().fat_entry_size, 2);
        assert_eq!(Geometry::compute(at_limit, 0x20).unwrap().fat_entry_size, 4);
    }

    #[test]
    fn fat_size_is_a_multiple_of_4096() {
        for mb in [17u64, 33, 100, 1000, 5000] {
            let g = Geometry::compute(mb * 1024 * 1024 + 512, 0x20).unwrap();
            assert_eq!(g.fat_size % 4096, 0);
            assert_eq!(g.data_offset % 4096, 0);
        }
    }

    #[test]
    fn last_cluster_fits_inside_the_partition() {
        for length in [20_000_000u64, 134_217_728, 5_000_000_000] {
            let g = Geometry::compute(length, 0x20).unwrap();
            let end = g.cluster_offset(g.cluster_count) + g.cluster_size as u64;
            assert!(end <= g.partition_length, "{length}: last cluster ends at {end}");
        }
    }

    #[test]
    fn picks_16k_clusters_until_the_console_limit() {
        assert_eq!(Geometry::pick_sectors_per_cluster(100 << 20), 0x20);
        assert_eq!(Geometry::pick_sectors_per_cluster(2 << 40), 0x20);
        assert_eq!(Geometry::pick_sectors_per_cluster(6 << 40), 0x40);
        assert_eq!(Geometry::pick_sectors_per_cluster(12 << 40), 0x80);
    }

    #[test]
    fn rejects_nonsense_cluster_sizes() {
        for spc in [0, 3, 129, 256] {
            assert!(Geometry::compute(1 << 30, spc).is_err(), "{spc}");
        }
    }

    #[test]
    fn tiny_partitions_are_refused() {
        assert!(Geometry::compute(0x2000, 0x20).is_err());
    }

    #[test]
    fn end_markers_follow_the_entry_size() {
        let small = Geometry::compute(256 << 20, 0x20).unwrap();
        assert_eq!(
            (small.end_of_chain(), small.media_entry(), small.end_threshold()),
            (0xFFFF, 0xFFF8, 0xFFF8)
        );
        let big = Geometry::compute(4 << 30, 0x20).unwrap();
        assert_eq!(big.end_of_chain(), 0xFFFF_FFFF);
        assert_eq!(big.bad_cluster(), 0xFFFF_FFF7);
    }
}
