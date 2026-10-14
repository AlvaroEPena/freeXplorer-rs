use std::collections::HashSet;

use super::Flavor;
use super::fat::FatTable;
use super::geometry::{Geometry, HEADER_SIZE, SECTOR_SIZE};
use super::node::{Attributes, Node, NodeId, NodeInfo};
use crate::device::{BlockDevice, Device};
use crate::error::{Error, ErrorKind, Result};

pub const MAX_ENTRIES_PER_DIRECTORY: usize = 0x1000;
pub const MAX_FILE_SIZE: u64 = u32::MAX as u64;
pub(super) const ENTRY_SIZE: usize = 64;

/// An open FATX partition.
///
/// FAT edits are held in memory and flushed before the matching directory entry is
/// written, so a crash can leave lost clusters behind (fsck finds those) but should
/// never leave an entry pointing at a chain that isn't on disk.
///
/// Everything takes `&mut self`. To share a volume between threads, put it in a mutex.
pub struct FatxVolume {
    pub(super) device: Device,
    pub(super) flavor: Flavor,
    pub(super) geometry: Geometry,
    pub(super) volume_id: u32,
    pub(super) read_only: bool,
    pub(super) fat: FatTable,
    pub(super) entries_per_cluster: usize,
    pub(super) nodes: Vec<Node>,
    pub(super) dirty: HashSet<NodeId>,
    pub(super) hint: u32,
    pub(super) free_clusters: u64,
    pub(super) free_known: bool,
    pub(super) root: NodeId,
}

impl FatxVolume {
    /// True when the first bytes say XTAF or FATX.
    pub fn has_signature(device: &dyn BlockDevice) -> bool {
        if device.len() < HEADER_SIZE {
            return false;
        }
        let mut head = [0u8; 4];
        device.read_at(0, &mut head).is_ok() && Flavor::from_signature(&head).is_some()
    }

    pub fn open(device: Device, writable: bool) -> Result<FatxVolume> {
        if device.len() < HEADER_SIZE + SECTOR_SIZE {
            return Err(Error::fatx(ErrorKind::NotFatx, "too small to be a FATX partition"));
        }
        let mut head = [0u8; 16];
        device.read_at(0, &mut head)?;
        let flavor = Flavor::from_signature(&head).ok_or_else(|| {
            Error::fatx(
                ErrorKind::NotFatx,
                "no XTAF or FATX signature at the start of the partition",
            )
        })?;

        let word = |at: usize| {
            let bytes = [head[at], head[at + 1], head[at + 2], head[at + 3]];
            if flavor.is_big_endian() {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        let (volume_id, spc, root_cluster) = (word(4), word(8), word(12));

        if spc > 128 || !Geometry::is_valid_sectors_per_cluster(spc) {
            return Err(Error::fatx(
                ErrorKind::NotFatx,
                format!("header says {spc} sectors per cluster, that can't be right"),
            ));
        }
        let geometry = Geometry::compute(device.len(), spc)?;
        if root_cluster < 1 || root_cluster > geometry.cluster_count {
            return Err(Error::fatx(
                ErrorKind::Corrupt,
                format!("root directory cluster {root_cluster} is out of range"),
            ));
        }

        let read_only = !(writable && device.can_write());
        let mut root = Node::new("", Attributes::DIRECTORY);
        root.first_cluster = root_cluster;
        Ok(FatxVolume {
            fat: FatTable::new(device.clone(), &geometry, flavor.is_big_endian()),
            entries_per_cluster: geometry.cluster_size as usize / ENTRY_SIZE,
            free_clusters: geometry.cluster_count as u64,
            device,
            flavor,
            geometry,
            volume_id,
            read_only,
            nodes: vec![root],
            dirty: HashSet::new(),
            hint: 1,
            free_known: false,
            root: NodeId(0),
        })
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    pub fn volume_id(&self) -> u32 {
        self.volume_id
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn cluster_size(&self) -> u32 {
        self.geometry.cluster_size
    }

    pub fn total_bytes(&self) -> u64 {
        self.geometry.cluster_count as u64 * self.geometry.cluster_size as u64
    }

    pub fn free_space_known(&self) -> bool {
        self.free_known
    }

    pub fn free_bytes(&self) -> u64 {
        self.free_clusters * self.geometry.cluster_size as u64
    }

    /// Walks the whole FAT once to count free clusters. Until this runs `free_bytes` is just
    /// the size of the volume. Cheap on small drives, a few seconds on a multi-TB one.
    pub fn scan_free_space(&mut self, progress: impl FnMut(f64)) -> Result<()> {
        self.free_clusters = self.fat.count_free(self.geometry.cluster_count, progress)?;
        self.free_known = true;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let pending: Vec<NodeId> = self.dirty.drain().collect();
        for id in pending {
            if !self.nodes[id.0].deleted {
                self.write_entry(id)?;
            }
        }
        self.fat.flush()?;
        self.device.flush()?;
        Ok(())
    }

    /// Writes just this node's directory entry if it has pending changes.
    pub fn flush_node(&mut self, id: NodeId) -> Result<()> {
        if self.dirty.remove(&id) && !self.nodes[id.0].deleted {
            self.write_entry(id)?;
        }
        Ok(())
    }

    /// Flushes and hands back any error, which dropping the volume can only swallow.
    pub fn close(mut self) -> Result<()> {
        let result = if self.read_only { Ok(()) } else { self.flush() };
        self.read_only = true; // nothing left for Drop to do
        result
    }

    // ---- small helpers shared by the other files ----

    /// The node with this number (see `NodeId::index`), if there is one that hasn't been deleted.
    /// For anything that gets numbers back from outside, like FUSE.
    pub fn node_at(&self, index: usize) -> Option<NodeId> {
        self.nodes.get(index).filter(|n| !n.deleted).map(|_| NodeId(index))
    }

    /// The folder something is in, None for the root.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.0].parent
    }

    pub fn info(&self, id: NodeId) -> NodeInfo {
        NodeInfo::from_node(id, &self.nodes[id.0], self.path_of(id), self.flavor)
    }

    pub fn path_of(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut at = id;
        while let Some(parent) = self.nodes[at.0].parent {
            parts.push(self.nodes[at.0].name.as_str());
            at = parent;
        }
        if parts.is_empty() {
            return "\\".to_string();
        }
        parts.reverse();
        format!("\\{}", parts.join("\\"))
    }

    pub(super) fn require_writable(&self) -> Result<()> {
        if self.read_only {
            return Err(Error::fatx(ErrorKind::ReadOnly, "the volume is mounted read-only"));
        }
        Ok(())
    }

    pub(super) fn require_directory(&self, id: NodeId) -> Result<()> {
        if !self.nodes[id.0].is_directory() {
            return Err(Error::fatx(
                ErrorKind::NotADirectory,
                format!("{} is not a directory", self.path_of(id)),
            ));
        }
        Ok(())
    }

    pub(super) fn require_file(&self, id: NodeId) -> Result<()> {
        if self.nodes[id.0].is_directory() {
            return Err(Error::fatx(
                ErrorKind::IsADirectory,
                format!("{} is a directory", self.path_of(id)),
            ));
        }
        Ok(())
    }

    pub(super) fn require_live(&self, id: NodeId) -> Result<()> {
        if self.nodes[id.0].deleted {
            return Err(Error::fatx(
                ErrorKind::NotFound,
                format!("{} was deleted", self.nodes[id.0].name),
            ));
        }
        Ok(())
    }

    pub(super) fn read_u32(&self, bytes: &[u8]) -> u32 {
        let word = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if self.flavor.is_big_endian() {
            u32::from_be_bytes(word)
        } else {
            u32::from_le_bytes(word)
        }
    }

    pub(super) fn write_u32(&self, bytes: &mut [u8], value: u32) {
        let word = if self.flavor.is_big_endian() {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        bytes[..4].copy_from_slice(&word);
    }

    pub(super) fn now(&self) -> u32 {
        super::time::now(self.flavor)
    }

    pub(super) fn alloc(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        NodeId(self.nodes.len() - 1)
    }

    // ---- the label ----

    /// The volume label lives in a file called name.txt in the root, UTF-16BE with a byte order mark.
    pub fn label(&mut self) -> Result<Option<String>> {
        self.ensure_listing(self.root)?;
        let Some(file) = self.nodes[self.root.0]
            .children
            .as_ref()
            .and_then(|l| l.get("name.txt"))
        else {
            return Ok(None);
        };
        let node = &self.nodes[file.0];
        if node.is_directory() || node.size < 2 || node.size > 128 {
            return Ok(None);
        }
        let mut raw = vec![0u8; node.size as usize];
        self.read(file, 0, &mut raw)?;
        let skip = if raw[0] == 0xFE && raw[1] == 0xFF { 2 } else { 0 };
        let units: Vec<u16> = raw[skip..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect();
        Ok(Some(
            String::from_utf16_lossy(&units).trim_end_matches('\0').to_string(),
        ))
    }

    pub fn set_label(&mut self, value: &str) -> Result<()> {
        self.require_writable()?;
        self.ensure_listing(self.root)?;
        let existing = self.nodes[self.root.0]
            .children
            .as_ref()
            .and_then(|l| l.get("name.txt"));
        if value.is_empty() {
            if let Some(old) = existing {
                self.delete(old)?;
            }
            return Ok(());
        }
        if value.chars().count() > 15 {
            return Err(Error::fatx(
                ErrorKind::InvalidName,
                "volume labels can be 15 characters at most",
            ));
        }
        let mut bytes = vec![0xFE, 0xFF];
        for unit in value.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        let file = match existing {
            Some(file) => file,
            None => self.create_file(self.root, "name.txt")?,
        };
        self.set_length(file, 0)?;
        self.write(file, 0, &bytes)
    }
}

impl Drop for FatxVolume {
    fn drop(&mut self) {
        if !self.read_only {
            let _ = self.flush();
        }
    }
}
