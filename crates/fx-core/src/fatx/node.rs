use std::collections::HashMap;

use chrono::NaiveDateTime;

use super::Flavor;
use super::cluster_map::ClusterMap;
use super::time;

/// A handle to a file or directory. It stays valid for as long as the volume is open, and
/// after the thing is deleted it just says so instead of pointing at something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) usize);

impl NodeId {
    /// A number that is different for every file and folder of a volume, handy as an inode.
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attributes(pub u8);

impl Attributes {
    pub const NONE: Attributes = Attributes(0);
    pub const READ_ONLY: Attributes = Attributes(0x01);
    pub const HIDDEN: Attributes = Attributes(0x02);
    pub const SYSTEM: Attributes = Attributes(0x04);
    pub const VOLUME: Attributes = Attributes(0x08);
    pub const DIRECTORY: Attributes = Attributes(0x10);
    pub const ARCHIVE: Attributes = Attributes(0x20);

    pub fn contains(self, other: Attributes) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Attributes {
    type Output = Attributes;
    fn bitor(self, rhs: Attributes) -> Attributes {
        Attributes(self.0 | rhs.0)
    }
}

pub(crate) struct Node {
    pub name: String,
    pub attributes: Attributes,
    pub first_cluster: u32,
    pub size: u64,
    pub parent: Option<NodeId>,
    pub deleted: bool,
    pub created: u32,
    pub modified: u32,
    pub accessed: u32,
    /// position in the parent's list of 64 byte entries
    pub slot: usize,
    pub children: Option<Listing>,
    pub map: Option<ClusterMap>,
}

impl Node {
    pub fn new(name: &str, attributes: Attributes) -> Node {
        Node {
            name: name.to_string(),
            attributes,
            first_cluster: 0,
            size: 0,
            parent: None,
            deleted: false,
            created: 0,
            modified: 0,
            accessed: 0,
            slot: usize::MAX,
            children: None,
            map: None,
        }
    }

    pub fn is_directory(&self) -> bool {
        self.attributes.contains(Attributes::DIRECTORY)
    }
}

/// What a directory looks like in memory: its clusters, and its 64 byte slots in order.
/// An empty slot is a deleted entry that can be reused.
pub(crate) struct Listing {
    pub clusters: Vec<u32>,
    pub slots: Vec<Option<NodeId>>,
    pub by_name: HashMap<String, NodeId>,
}

impl Listing {
    pub fn new() -> Listing {
        Listing {
            clusters: Vec::new(),
            slots: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    /// Names match without regard to case.
    pub fn key(name: &str) -> String {
        name.to_lowercase()
    }

    pub fn get(&self, name: &str) -> Option<NodeId> {
        self.by_name.get(&Self::key(name)).copied()
    }
}

/// A copy of what is known about a file or directory at the moment it was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub id: NodeId,
    pub name: String,
    pub path: String,
    pub attributes: Attributes,
    pub size: u64,
    pub first_cluster: u32,
    pub created: NaiveDateTime,
    pub modified: NaiveDateTime,
    pub accessed: NaiveDateTime,
    pub is_directory: bool,
    pub is_root: bool,
    pub is_deleted: bool,
}

impl NodeInfo {
    pub(crate) fn from_node(id: NodeId, node: &Node, path: String, flavor: Flavor) -> NodeInfo {
        NodeInfo {
            id,
            name: node.name.clone(),
            path,
            attributes: node.attributes,
            size: node.size,
            first_cluster: node.first_cluster,
            created: time::decode(node.created, flavor),
            modified: time::decode(node.modified, flavor),
            accessed: time::decode(node.accessed, flavor),
            is_directory: node.is_directory(),
            is_root: node.parent.is_none(),
            is_deleted: node.deleted,
        }
    }
}
