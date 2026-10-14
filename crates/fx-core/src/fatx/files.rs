use super::cluster_map::ClusterMap;
use super::node::NodeId;
use super::volume::{FatxVolume, MAX_FILE_SIZE};
use crate::error::{Error, ErrorKind, Result};

const MAX_IO_CHUNK: u64 = 8 << 20;

impl FatxVolume {
    /// Reads from `offset` into `dest` and gives back how many bytes that was. Reading
    /// at or past the end gives 0.
    pub fn read(&mut self, file: NodeId, offset: u64, dest: &mut [u8]) -> Result<usize> {
        self.require_file(file)?;
        self.require_live(file)?;
        let size = self.nodes[file.0].size;
        if offset >= size || dest.is_empty() {
            return Ok(0);
        }

        let mut remaining = (dest.len() as u64).min(size - offset);
        self.ensure_map(file)?;
        let cs = self.geometry.cluster_size as u64;
        let mut done = 0usize;
        while remaining > 0 {
            let pos = offset + done as u64;
            let (disk, run) = self.nodes[file.0].map.as_ref().unwrap().find(pos / cs).ok_or_else(|| {
                Error::fatx(
                    ErrorKind::Corrupt,
                    format!(
                        "{}: the cluster chain is shorter than the file size says",
                        self.path_of(file)
                    ),
                )
            })?;
            let within = pos % cs;
            let n = remaining.min(run * cs - within).min(MAX_IO_CHUNK) as usize;
            self.device
                .read_at(self.geometry.cluster_offset(disk) + within, &mut dest[done..done + n])?;
            done += n;
            remaining -= n as u64;
        }
        Ok(done)
    }

    /// Writing past the end grows the file. Anything between the old end and the offset
    /// is zeroed, like every other file system.
    pub fn write(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<()> {
        self.require_writable()?;
        self.require_file(file)?;
        self.require_live(file)?;
        if data.is_empty() {
            return Ok(());
        }

        let end = offset + data.len() as u64;
        if end > MAX_FILE_SIZE {
            return Err(Error::fatx(
                ErrorKind::FileTooLarge,
                "FATX files can't be bigger than 4 GB",
            ));
        }

        self.ensure_map(file)?;
        let cs = self.geometry.cluster_size as u64;
        let old_size = self.nodes[file.0].size;
        let needed = end.div_ceil(cs);
        let mut grew = false;

        let have = self.nodes[file.0].map.as_ref().unwrap().count();
        if needed > have {
            self.extend(file, needed - have)?;
            grew = true;
        }
        if offset > old_size {
            self.zero_range(file, old_size, offset - old_size)?;
        }

        self.write_range(file, offset, data)?;

        if end > old_size {
            self.nodes[file.0].size = end;
            grew = true;
        }
        let now = self.now();
        self.nodes[file.0].modified = now;
        self.nodes[file.0].accessed = now;

        if grew {
            self.fat.flush()?;
            self.write_entry(file)?;
            self.dirty.remove(&file);
        } else {
            self.dirty.insert(file);
        }
        Ok(())
    }

    pub fn set_length(&mut self, file: NodeId, length: u64) -> Result<()> {
        self.require_writable()?;
        self.require_file(file)?;
        self.require_live(file)?;
        if length > MAX_FILE_SIZE {
            return Err(Error::fatx(
                ErrorKind::FileTooLarge,
                "FATX files can't be bigger than 4 GB",
            ));
        }
        let old = self.nodes[file.0].size;
        if length == old {
            return Ok(());
        }

        self.ensure_map(file)?;
        let cs = self.geometry.cluster_size as u64;
        let needed = length.div_ceil(cs);
        let have = self.nodes[file.0].map.as_ref().unwrap().count();
        let now = self.now();

        if length > old {
            if needed > have {
                self.extend(file, needed - have)?;
            }
            self.zero_range(file, old, length - old)?;
            let node = &mut self.nodes[file.0];
            node.size = length;
            node.modified = now;
            node.accessed = now;
            self.fat.flush()?;
            self.write_entry(file)?;
        } else {
            let node = &mut self.nodes[file.0];
            node.size = length;
            node.modified = now;
            node.accessed = now;
            if needed < have {
                let tail = self.nodes[file.0].map.as_ref().unwrap().runs_from(needed);
                if needed == 0 {
                    self.nodes[file.0].first_cluster = 0;
                } else {
                    let keep = self.nodes[file.0].map.as_ref().unwrap().cluster_at(needed - 1).unwrap();
                    self.set_fat(keep, self.geometry.end_of_chain())?;
                }
                // entry first: if we die before the FAT is flushed the chain is just longer than the size
                self.write_entry(file)?;
                for (disk, count) in tail {
                    for i in 0..count {
                        let c = disk + i as u32;
                        self.set_fat(c, 0)?;
                        if c < self.hint {
                            self.hint = c;
                        }
                    }
                }
                self.nodes[file.0].map.as_mut().unwrap().truncate(needed);
                self.fat.flush()?;
            } else {
                self.write_entry(file)?;
            }
        }
        self.dirty.remove(&file);
        Ok(())
    }

    // ---- allocation ----

    pub(super) fn ensure_map(&mut self, id: NodeId) -> Result<()> {
        if self.nodes[id.0].map.is_none() {
            let map = self.build_map(id)?;
            self.nodes[id.0].map = Some(map);
        }
        Ok(())
    }

    fn build_map(&mut self, id: NodeId) -> Result<ClusterMap> {
        let mut map = ClusterMap::new();
        let mut c = self.nodes[id.0].first_cluster;
        if c == 0 {
            return Ok(map);
        }

        let count = self.geometry.cluster_count;
        let mut guard = 0u64;
        loop {
            if c < 1 || c > count {
                return Err(Error::fatx(
                    ErrorKind::Corrupt,
                    format!("{}: chain points at cluster {c}, outside the volume", self.path_of(id)),
                ));
            }
            guard += 1;
            if guard > count as u64 {
                return Err(Error::fatx(
                    ErrorKind::Corrupt,
                    format!("{}: chain loops back on itself", self.path_of(id)),
                ));
            }
            map.append(c);

            let next = self.fat.get(c)?;
            if next >= self.geometry.end_threshold() {
                break;
            }
            if next == 0 || next == self.geometry.bad_cluster() {
                return Err(Error::fatx(
                    ErrorKind::Corrupt,
                    format!("{}: chain runs into a free or bad cluster at {c}", self.path_of(id)),
                ));
            }
            c = next;
        }
        Ok(map)
    }

    pub(super) fn set_fat(&mut self, index: u32, value: u32) -> Result<()> {
        if self.free_known {
            let old = self.fat.get(index)?;
            if old == 0 && value != 0 {
                self.free_clusters = self.free_clusters.saturating_sub(1);
            } else if old != 0 && value == 0 {
                self.free_clusters += 1;
            }
        }
        self.fat.set(index, value)
    }

    /// Next-fit search, so repeated allocations tend to land one after the other.
    fn find_free(&mut self, count: u64, start_at: u32) -> Result<Option<Vec<(u32, u32)>>> {
        let limit = self.geometry.cluster_count;
        let start_at = if start_at < 1 || start_at > limit { 1 } else { start_at };

        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut need = count;
        let mut i = start_at;
        let mut scanned = 0u64;
        while scanned < limit as u64 && need > 0 {
            if self.fat.get(i)? == 0 {
                match runs.last_mut() {
                    Some(last) if last.0 as u64 + last.1 as u64 == i as u64 => last.1 += 1,
                    _ => runs.push((i, 1)),
                }
                need -= 1;
            }
            i = if i == limit { 1 } else { i + 1 };
            scanned += 1;
        }
        Ok(if need > 0 { None } else { Some(runs) })
    }

    /// Adds clusters to the end of a node's chain. All or nothing.
    pub(super) fn extend(&mut self, id: NodeId, count: u64) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let full = || Error::fatx(ErrorKind::DiskFull, "there isn't enough free space on the volume");
        if self.free_known && count > self.free_clusters {
            return Err(full());
        }

        let (have, last) = {
            let map = self.nodes[id.0]
                .map
                .as_ref()
                .expect("the map is built before a chain grows");
            (map.count(), map.last_cluster())
        };
        let start_at = if have > 0 { last + 1 } else { self.hint };
        let runs = self.find_free(count, start_at)?.ok_or_else(full)?;

        let eoc = self.geometry.end_of_chain();
        for (r, &(start, length)) in runs.iter().enumerate() {
            for c in start..start + length - 1 {
                self.set_fat(c, c + 1)?;
            }
            let next = runs.get(r + 1).map_or(eoc, |next| next.0);
            self.set_fat(start + length - 1, next)?;
        }
        if have > 0 {
            self.set_fat(last, runs[0].0)?;
        } else {
            self.nodes[id.0].first_cluster = runs[0].0;
        }

        let map = self.nodes[id.0].map.as_mut().unwrap();
        for &(start, length) in &runs {
            map.append_run(start, length as u64);
        }

        let (last_start, last_length) = *runs.last().unwrap();
        self.hint = last_start + last_length;
        if self.hint > self.geometry.cluster_count {
            self.hint = 1;
        }
        Ok(())
    }

    /// Frees a whole chain by following the FAT. Broken chains are tolerated, it just stops.
    pub(super) fn free_chain(&mut self, first: u32) -> Result<()> {
        let count = self.geometry.cluster_count;
        let mut c = first;
        let mut guard = 0u64;
        while c >= 1 && c <= count && guard <= count as u64 {
            guard += 1;
            let next = self.fat.get(c)?;
            self.set_fat(c, 0)?;
            if c < self.hint {
                self.hint = c;
            }
            if next >= self.geometry.end_threshold() || next == 0 {
                break;
            }
            c = next;
        }
        Ok(())
    }

    // ---- raw data movement inside an already allocated chain ----

    fn write_range(&mut self, id: NodeId, offset: u64, data: &[u8]) -> Result<()> {
        let cs = self.geometry.cluster_size as u64;
        let mut done = 0usize;
        while done < data.len() {
            let pos = offset + done as u64;
            let (disk, run) = self.nodes[id.0]
                .map
                .as_ref()
                .unwrap()
                .find(pos / cs)
                .ok_or_else(|| Error::fatx(ErrorKind::Corrupt, "tried to write outside the allocated clusters"))?;
            let within = pos % cs;
            let n = ((data.len() - done) as u64).min(run * cs - within).min(MAX_IO_CHUNK) as usize;
            self.device
                .write_at(self.geometry.cluster_offset(disk) + within, &data[done..done + n])?;
            done += n;
        }
        Ok(())
    }

    fn zero_range(&mut self, id: NodeId, start: u64, length: u64) -> Result<()> {
        if length == 0 {
            return Ok(());
        }
        let cs = self.geometry.cluster_size as u64;
        let zeros = vec![0u8; length.min(1 << 20) as usize];
        let mut pos = start;
        let mut left = length;
        while left > 0 {
            let (disk, run) = self.nodes[id.0]
                .map
                .as_ref()
                .unwrap()
                .find(pos / cs)
                .ok_or_else(|| Error::fatx(ErrorKind::Corrupt, "tried to zero outside the allocated clusters"))?;
            let within = pos % cs;
            let n = left.min(run * cs - within).min(zeros.len() as u64) as usize;
            self.device
                .write_at(self.geometry.cluster_offset(disk) + within, &zeros[..n])?;
            pos += n as u64;
            left -= n as u64;
        }
        Ok(())
    }
}
