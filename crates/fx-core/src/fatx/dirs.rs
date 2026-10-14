use super::cluster_map::ClusterMap;
use super::name::{self, MAX_LENGTH, MAX_PATH_LENGTH};
use super::node::{Attributes, Listing, Node, NodeId, NodeInfo};
use super::volume::{ENTRY_SIZE, FatxVolume, MAX_ENTRIES_PER_DIRECTORY};
use super::{Flavor, time};
use crate::error::{Error, ErrorKind, Result};

impl FatxVolume {
    // ---- lookups ----

    pub fn children(&mut self, directory: NodeId) -> Result<Vec<NodeInfo>> {
        self.require_directory(directory)?;
        self.require_live(directory)?;
        self.ensure_listing(directory)?;
        let ids: Vec<NodeId> = self.listing(directory).slots.iter().flatten().copied().collect();
        Ok(ids.into_iter().map(|id| self.info(id)).collect())
    }

    pub(crate) fn child_ids(&mut self, directory: NodeId) -> Result<Vec<NodeId>> {
        self.ensure_listing(directory)?;
        Ok(self.listing(directory).slots.iter().flatten().copied().collect())
    }

    pub fn find_child(&mut self, directory: NodeId, name: &str) -> Result<Option<NodeId>> {
        self.require_directory(directory)?;
        self.require_live(directory)?;
        self.ensure_listing(directory)?;
        Ok(self.listing(directory).get(name))
    }

    /// Accepts either slash. Gives None when something along the way is missing.
    pub fn find(&mut self, path: &str) -> Result<Option<NodeId>> {
        let mut node = self.root;
        for part in name::split_path(path) {
            if !self.nodes[node.0].is_directory() {
                return Ok(None);
            }
            self.ensure_listing(node)?;
            match self.listing(node).get(part) {
                Some(next) => node = next,
                None => return Ok(None),
            }
        }
        Ok(Some(node))
    }

    pub fn resolve(&mut self, path: &str) -> Result<NodeId> {
        self.find(path)?
            .ok_or_else(|| Error::fatx(ErrorKind::NotFound, format!("{path} doesn't exist")))
    }

    // ---- changes ----

    /// The console doesn't set the archive bit on files it creates, so neither does this.
    pub fn create_file(&mut self, parent: NodeId, name: &str) -> Result<NodeId> {
        self.create(parent, name, Attributes::NONE)
    }

    pub fn create_directory(&mut self, parent: NodeId, name: &str) -> Result<NodeId> {
        self.create(parent, name, Attributes::DIRECTORY)
    }

    /// Makes every missing directory along the path and returns the last one.
    pub fn create_directories(&mut self, path: &str) -> Result<NodeId> {
        let mut node = self.root;
        for part in name::split_path(path) {
            self.ensure_listing(node)?;
            node = match self.listing(node).get(part) {
                Some(next) if self.nodes[next.0].is_directory() => next,
                Some(next) => {
                    return Err(Error::fatx(
                        ErrorKind::NotADirectory,
                        format!("{} is a file", self.path_of(next)),
                    ));
                }
                None => self.create_directory(node, part)?,
            };
        }
        Ok(node)
    }

    fn create(&mut self, parent: NodeId, name: &str, attributes: Attributes) -> Result<NodeId> {
        self.require_writable()?;
        self.require_directory(parent)?;
        self.require_live(parent)?;
        name::require(name)?;
        if self.path_of(parent).trim_end_matches('\\').len() + 1 + name.len() > MAX_PATH_LENGTH {
            return Err(Error::fatx(
                ErrorKind::PathTooLong,
                "paths on FATX can't be longer than 240 characters",
            ));
        }
        self.ensure_listing(parent)?;
        if self.listing(parent).get(name).is_some() {
            return Err(Error::fatx(ErrorKind::AlreadyExists, format!("{name} already exists")));
        }

        // find the slot first so a full directory fails before anything is allocated
        let slot = self.reserve_slot(parent)?;

        let mut node = Node::new(name, attributes);
        let now = self.now();
        node.created = now;
        node.modified = now;
        node.accessed = now;
        let is_directory = node.is_directory();
        let id = self.alloc(node);

        if is_directory {
            self.nodes[id.0].map = Some(ClusterMap::new());
            self.extend(id, 1)?;
            let blank = vec![0xFFu8; self.geometry.cluster_size as usize];
            self.device
                .write_at(self.geometry.cluster_offset(self.nodes[id.0].first_cluster), &blank)?;
            self.fat.flush()?;
        }

        self.attach(parent, id, slot)?;
        Ok(id)
    }

    pub fn delete(&mut self, id: NodeId) -> Result<()> {
        self.require_writable()?;
        self.require_live(id)?;
        if self.nodes[id.0].parent.is_none() {
            return Err(Error::fatx(
                ErrorKind::InvalidOperation,
                "can't delete the root directory",
            ));
        }
        if self.nodes[id.0].is_directory() {
            self.ensure_listing(id)?;
            if !self.listing(id).by_name.is_empty() {
                return Err(Error::fatx(
                    ErrorKind::DirectoryNotEmpty,
                    format!("{} isn't empty", self.path_of(id)),
                ));
            }
        }

        self.detach(id)?;
        self.nodes[id.0].deleted = true;
        self.dirty.remove(&id);
        let first = self.nodes[id.0].first_cluster;
        self.free_chain(first)?;
        self.nodes[id.0].first_cluster = 0;
        self.nodes[id.0].map = None;
        self.fat.flush()
    }

    /// Moves and/or renames. `replace_existing` only ever replaces a plain file.
    pub fn rename(&mut self, id: NodeId, new_parent: NodeId, new_name: &str, replace_existing: bool) -> Result<()> {
        self.require_writable()?;
        self.require_live(id)?;
        self.require_directory(new_parent)?;
        self.require_live(new_parent)?;
        let Some(old_parent) = self.nodes[id.0].parent else {
            return Err(Error::fatx(
                ErrorKind::InvalidOperation,
                "can't rename the root directory",
            ));
        };
        name::require(new_name)?;
        let below = if self.nodes[id.0].is_directory() {
            self.deepest_below(id)?
        } else {
            0
        };
        if self.path_of(new_parent).trim_end_matches('\\').len() + 1 + new_name.len() + below > MAX_PATH_LENGTH {
            return Err(Error::fatx(
                ErrorKind::PathTooLong,
                "paths on FATX can't be longer than 240 characters",
            ));
        }

        if self.nodes[id.0].is_directory() {
            let mut at = Some(new_parent);
            while let Some(p) = at {
                if p == id {
                    return Err(Error::fatx(
                        ErrorKind::InvalidOperation,
                        "can't move a directory into itself",
                    ));
                }
                at = self.nodes[p.0].parent;
            }
        }

        self.ensure_listing(new_parent)?;
        let same_dir = new_parent == old_parent;

        if let Some(existing) = self.listing(new_parent).get(new_name).filter(|&e| e != id) {
            if !replace_existing {
                return Err(Error::fatx(
                    ErrorKind::AlreadyExists,
                    format!("{new_name} already exists"),
                ));
            }
            if self.nodes[existing.0].is_directory() || self.nodes[id.0].is_directory() {
                return Err(Error::fatx(
                    ErrorKind::InvalidOperation,
                    "a directory can't replace or be replaced",
                ));
            }
            self.delete(existing)?;
        }

        if same_dir {
            let old_key = Listing::key(&self.nodes[id.0].name);
            let listing = self.nodes[new_parent.0].children.as_mut().unwrap();
            listing.by_name.remove(&old_key);
            listing.by_name.insert(Listing::key(new_name), id);
            self.nodes[id.0].name = new_name.to_string();
            return self.write_entry(id);
        }

        let slot = self.reserve_slot(new_parent)?;
        self.ensure_listing(old_parent)?;
        let old_slot = self.nodes[id.0].slot;
        let old_name = self.nodes[id.0].name.clone();

        self.nodes[id.0].name = new_name.to_string();
        self.attach(new_parent, id, slot)?;

        // the new entry is on disk by now, so retire the old one
        let offset = self.slot_offset(old_parent, old_slot);
        self.device.write_at(offset, &[0xE5])?;
        let old_listing = self.nodes[old_parent.0].children.as_mut().unwrap();
        old_listing.slots[old_slot] = None;
        old_listing.by_name.remove(&Listing::key(&old_name));
        Ok(())
    }

    pub fn set_attributes(&mut self, id: NodeId, attributes: Attributes) -> Result<()> {
        self.require_writable()?;
        self.require_live(id)?;
        if self.nodes[id.0].parent.is_none() {
            return Ok(());
        }
        let keep = self.nodes[id.0].attributes.0 & (Attributes::DIRECTORY.0 | Attributes::VOLUME.0);
        let change = attributes.0 & !(Attributes::DIRECTORY.0 | Attributes::VOLUME.0);
        self.nodes[id.0].attributes = Attributes(keep | change);
        self.write_entry(id)
    }

    pub fn set_times(
        &mut self,
        id: NodeId,
        created: Option<chrono::NaiveDateTime>,
        modified: Option<chrono::NaiveDateTime>,
        accessed: Option<chrono::NaiveDateTime>,
    ) -> Result<()> {
        self.require_writable()?;
        self.require_live(id)?;
        if self.nodes[id.0].parent.is_none() {
            return Ok(());
        }
        let flavor = self.flavor;
        let node = &mut self.nodes[id.0];
        if let Some(c) = created {
            node.created = time::encode(c, flavor);
        }
        if let Some(m) = modified {
            node.modified = time::encode(m, flavor);
        }
        if let Some(a) = accessed {
            node.accessed = time::encode(a, flavor);
        }
        self.write_entry(id)
    }

    // ---- directory plumbing ----

    /// Length of the longest path under a directory, counted from the directory itself.
    fn deepest_below(&mut self, directory: NodeId) -> Result<usize> {
        let mut deepest = 0;
        let mut pending = vec![(directory, 0usize)];
        while let Some((dir, length)) = pending.pop() {
            for child in self.child_ids(dir)? {
                let child_length = length + 1 + self.nodes[child.0].name.len();
                deepest = deepest.max(child_length);
                if self.nodes[child.0].is_directory() {
                    pending.push((child, child_length));
                }
            }
        }
        Ok(deepest)
    }

    pub(super) fn listing(&self, directory: NodeId) -> &Listing {
        self.nodes[directory.0]
            .children
            .as_ref()
            .expect("listing is loaded before it's used")
    }

    pub(super) fn ensure_listing(&mut self, directory: NodeId) -> Result<()> {
        if self.nodes[directory.0].children.is_none() {
            self.load_listing(directory)?;
        }
        Ok(())
    }

    fn load_listing(&mut self, directory: NodeId) -> Result<()> {
        self.ensure_map(directory)?;
        let mut listing = Listing::new();
        for e in self.nodes[directory.0].map.as_ref().unwrap().extents() {
            for i in 0..e.length {
                listing.clusters.push(e.disk + i as u32);
            }
        }

        let mut buffer = vec![0u8; self.geometry.cluster_size as usize];
        'clusters: for &cluster in &listing.clusters.clone() {
            self.device
                .read_at(self.geometry.cluster_offset(cluster), &mut buffer)?;
            for i in 0..self.entries_per_cluster {
                let raw = &buffer[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE];
                let len = raw[0];
                if len == 0x00 || len == 0xFF {
                    break 'clusters;
                }

                let slot = listing.slots.len();
                if len == 0xE5 || len as usize > MAX_LENGTH {
                    // deleted, or junk that can't be used. the slot stays free
                    listing.slots.push(None);
                    continue;
                }

                let mut node = self.parse_entry(raw, len as usize);
                node.parent = Some(directory);
                node.slot = slot;
                let name = node.name.clone();
                let id = self.alloc(node);
                listing.slots.push(Some(id));
                // two entries with the same name shouldn't happen. the first one wins.
                listing.by_name.entry(Listing::key(&name)).or_insert(id);
            }
        }
        self.nodes[directory.0].children = Some(listing);
        Ok(())
    }

    fn parse_entry(&self, raw: &[u8], name_length: usize) -> Node {
        let name: String = raw[2..2 + name_length].iter().map(|&b| b as char).collect();
        let mut node = Node::new(&name, Attributes(raw[1]));
        node.first_cluster = self.read_u32(&raw[0x2C..]);
        node.size = self.read_u32(&raw[0x30..]) as u64;
        if self.flavor == Flavor::Xbox360 {
            node.created = self.read_u32(&raw[0x34..]);
            node.modified = self.read_u32(&raw[0x38..]);
        } else {
            node.modified = self.read_u32(&raw[0x34..]);
            node.created = self.read_u32(&raw[0x38..]);
        }
        node.accessed = self.read_u32(&raw[0x3C..]);
        if node.is_directory() {
            node.size = 0;
        }
        node
    }

    fn build_entry(&self, node: &Node, raw: &mut [u8]) {
        raw.fill(0xFF);
        let name: Vec<u8> = node.name.chars().map(|c| c as u8).collect();
        raw[0] = name.len() as u8;
        raw[1] = node.attributes.0;
        raw[2..2 + name.len()].copy_from_slice(&name);
        self.write_u32(&mut raw[0x2C..], node.first_cluster);
        self.write_u32(&mut raw[0x30..], if node.is_directory() { 0 } else { node.size as u32 });
        if self.flavor == Flavor::Xbox360 {
            self.write_u32(&mut raw[0x34..], node.created);
            self.write_u32(&mut raw[0x38..], node.modified);
        } else {
            self.write_u32(&mut raw[0x34..], node.modified);
            self.write_u32(&mut raw[0x38..], node.created);
        }
        self.write_u32(&mut raw[0x3C..], node.accessed);
    }

    pub(super) fn slot_offset(&self, directory: NodeId, slot: usize) -> u64 {
        let listing = self.listing(directory);
        self.geometry
            .cluster_offset(listing.clusters[slot / self.entries_per_cluster])
            + (slot % self.entries_per_cluster) as u64 * ENTRY_SIZE as u64
    }

    pub(super) fn write_entry(&mut self, id: NodeId) -> Result<()> {
        let node = &self.nodes[id.0];
        let Some(parent) = node.parent else { return Ok(()) };
        if node.deleted {
            return Ok(());
        }
        self.ensure_listing(parent)?;
        let mut raw = [0u8; ENTRY_SIZE];
        self.build_entry(&self.nodes[id.0], &mut raw);
        let offset = self.slot_offset(parent, self.nodes[id.0].slot);
        self.device.write_at(offset, &raw)?;
        Ok(())
    }

    /// Picks the slot a new entry will go in: the first deleted one, or the end of the
    /// directory. Adds a cluster to the directory if it's out of room.
    fn reserve_slot(&mut self, directory: NodeId) -> Result<usize> {
        self.ensure_listing(directory)?;
        if let Some(free) = self.listing(directory).slots.iter().position(Option::is_none) {
            return Ok(free);
        }

        let slot = self.listing(directory).slots.len();
        if slot >= MAX_ENTRIES_PER_DIRECTORY {
            return Err(Error::fatx(
                ErrorKind::DirectoryFull,
                format!(
                    "{} already has the maximum {MAX_ENTRIES_PER_DIRECTORY} entries",
                    self.path_of(directory)
                ),
            ));
        }

        if slot >= self.listing(directory).clusters.len() * self.entries_per_cluster {
            self.ensure_map(directory)?;
            self.extend(directory, 1)?;
            let added = self.nodes[directory.0].map.as_ref().unwrap().last_cluster();
            let blank = vec![0xFFu8; self.geometry.cluster_size as usize];
            self.device.write_at(self.geometry.cluster_offset(added), &blank)?;
            self.fat.flush()?;
            self.nodes[directory.0].children.as_mut().unwrap().clusters.push(added);
        }
        Ok(slot)
    }

    fn attach(&mut self, parent: NodeId, id: NodeId, slot: usize) -> Result<()> {
        self.nodes[id.0].parent = Some(parent);
        self.nodes[id.0].slot = slot;
        let key = Listing::key(&self.nodes[id.0].name);
        let listing = self.nodes[parent.0].children.as_mut().unwrap();
        if slot == listing.slots.len() {
            listing.slots.push(Some(id));
        } else {
            listing.slots[slot] = Some(id);
        }
        listing.by_name.insert(key, id);
        self.write_entry(id)
    }

    /// Takes a node out of its parent's listing and marks the slot deleted on disk.
    fn detach(&mut self, id: NodeId) -> Result<()> {
        let parent = self.nodes[id.0].parent.expect("only the root has no parent");
        self.ensure_listing(parent)?;
        let slot = self.nodes[id.0].slot;
        let offset = self.slot_offset(parent, slot);
        self.device.write_at(offset, &[0xE5])?;
        let key = Listing::key(&self.nodes[id.0].name);
        let listing = self.nodes[parent.0].children.as_mut().unwrap();
        listing.slots[slot] = None;
        listing.by_name.remove(&key);
        Ok(())
    }
}
