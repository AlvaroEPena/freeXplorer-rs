//! A partition on a console drive, whichever console it came from.
//!
//! The Xbox 360 layout is fixed and compiled in, the original Xbox layout comes off the
//! drive's own partition table, but once a partition has been detected the rest of the
//! program only needs these two types.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partition_ends_where_its_length_says() {
        let spec = PartitionSpec {
            id: "x",
            name: "X",
            offset: 0x80000,
            length: 0x1000,
            file_system: PartitionFileSystem::Fatx,
            description: "",
        };
        assert_eq!(spec.end(), 0x81000);
        assert!(spec.is_fatx());
    }
}
