use super::name;
use super::node::NodeId;
use super::volume::{FatxVolume, MAX_ENTRIES_PER_DIRECTORY};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckIssue {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct FsckReport {
    pub issues: Vec<FsckIssue>,
    pub directories: u64,
    pub files: u64,
    pub file_bytes: u64,
    pub used_clusters: u64,
    pub orphan_clusters: u64,
    pub fixed_clusters: u64,
}

impl FsckReport {
    pub fn errors(&self) -> usize {
        self.issues.iter().filter(|i| i.severity == Severity::Error).count()
    }

    pub fn warnings(&self) -> usize {
        self.issues.iter().filter(|i| i.severity == Severity::Warning).count()
    }

    pub fn is_clean(&self) -> bool {
        self.errors() == 0 && self.warnings() == 0
    }

    fn add(&mut self, severity: Severity, message: String) {
        // a damaged volume can produce millions of these, keep the report readable
        if self.issues.len() < 500 {
            self.issues.push(FsckIssue { severity, message });
        } else if self.issues.len() == 500 {
            self.issues.push(FsckIssue {
                severity: Severity::Info,
                message: "more problems found, not listing them all".into(),
            });
        }
    }
}

struct Owned {
    bits: Vec<u64>,
    count: u64,
}

impl Owned {
    fn test(&self, c: u32) -> bool {
        self.bits[(c >> 6) as usize] & (1u64 << (c & 63)) != 0
    }

    fn mark(&mut self, c: u32) {
        self.bits[(c >> 6) as usize] |= 1u64 << (c & 63);
        self.count += 1;
    }
}

impl FatxVolume {
    /// A quick look at the first root directory cluster. If the geometry is off, that read
    /// lands somewhere else entirely and you get zeros or file data instead of entries.
    pub(crate) fn root_looks_sane(&mut self) -> bool {
        let mut buffer = vec![0u8; self.geometry.cluster_size as usize];
        let root_cluster = self.nodes[self.root.0].first_cluster;
        if self
            .device
            .read_at(self.geometry.cluster_offset(root_cluster), &mut buffer)
            .is_err()
        {
            return false;
        }
        for i in 0..self.entries_per_cluster {
            let e = &buffer[i * 64..(i + 1) * 64];
            let len = e[0];
            match len {
                0xFF => return true,  // blank, or the end of the entries
                0x00 => return i > 0, // zeros right at the start is what a misplaced read looks like
                0xE5 => continue,
                _ => {}
            }
            if len as usize > name::MAX_LENGTH {
                return false;
            }
            if e[2..2 + len as usize].iter().any(|&b| !(0x20..=0x7E).contains(&b)) {
                return false;
            }
            if self.read_u32(&e[0x2C..]) > self.geometry.cluster_count {
                return false;
            }
        }
        true
    }

    /// Walks the whole tree and the whole FAT and reports what doesn't add up. With
    /// `free_orphans` set, clusters that are marked used but that no file owns get released.
    /// It re-reads every directory from disk, so handles handed out earlier are stale afterwards.
    pub fn check(&mut self, free_orphans: bool, mut status: impl FnMut(&str)) -> Result<FsckReport> {
        let mut report = FsckReport::default();
        let count = self.geometry.cluster_count;
        let mut owned = Owned {
            bits: vec![0u64; (count as usize + 64) / 64],
            count: 0,
        };

        let root = self.root;
        let mut pending: Vec<NodeId> = vec![root];
        let first = self.nodes[root.0].first_cluster;
        if self.walk("\\", first, &mut owned, &mut report)? < 0 {
            pending.clear();
        }

        while let Some(dir) = pending.pop() {
            let dir_path = self.path_of(dir);
            status(&dir_path);
            report.directories += 1;

            self.nodes[dir.0].map = None;
            self.nodes[dir.0].children = None;
            let children = match self.child_ids(dir) {
                Ok(children) => children,
                Err(err @ crate::Error::Fatx { .. }) => {
                    report.add(Severity::Error, format!("{dir_path}: can't read the directory ({err})"));
                    continue;
                }
                Err(other) => return Err(other),
            };

            let slots = self.listing(dir).slots.len();
            if slots > MAX_ENTRIES_PER_DIRECTORY {
                report.add(
                    Severity::Warning,
                    format!("{dir_path}: has {slots} entries, the console stops at {MAX_ENTRIES_PER_DIRECTORY}"),
                );
            }

            for child in children {
                let path = self.path_of(child);
                let (child_name, is_dir, first_cluster, size) = {
                    let n = &self.nodes[child.0];
                    (n.name.clone(), n.is_directory(), n.first_cluster, n.size)
                };

                if !name::is_valid(&child_name) {
                    report.add(
                        Severity::Warning,
                        format!("{path}: the name has characters the console may not like"),
                    );
                }

                if is_dir {
                    if first_cluster == 0 {
                        report.add(Severity::Error, format!("{path}: directory has no cluster"));
                        continue;
                    }
                    if self.walk(&path, first_cluster, &mut owned, &mut report)? >= 0 {
                        pending.push(child);
                    }
                    continue;
                }

                report.files += 1;
                report.file_bytes += size;
                let cs = self.geometry.cluster_size as u64;
                let needed = size.div_ceil(cs) as i64;

                if first_cluster == 0 {
                    if size > 0 {
                        report.add(Severity::Error, format!("{path}: has {size} bytes but no clusters"));
                    }
                    continue;
                }

                let have = self.walk(&path, first_cluster, &mut owned, &mut report)?;
                if have < 0 {
                    continue;
                }
                if have < needed {
                    report.add(
                        Severity::Error,
                        format!("{path}: size needs {needed} clusters but the chain has {have}"),
                    );
                } else if have > needed {
                    report.add(
                        Severity::Warning,
                        format!("{path}: chain has {} more clusters than the size needs", have - needed),
                    );
                }
            }
        }

        report.used_clusters = owned.count;

        // anything the FAT calls used that nobody owns
        let fix = free_orphans && !self.read_only;
        let mut orphans = 0u64;
        for c in 1..=count {
            if owned.test(c) || self.fat.get(c)? == 0 {
                continue;
            }
            orphans += 1;
            if fix {
                self.set_fat(c, 0)?;
                report.fixed_clusters += 1;
            }
        }
        report.orphan_clusters = orphans;
        if orphans > 0 {
            let tail = if fix { " (released)" } else { "" };
            report.add(
                Severity::Warning,
                format!("{orphans} clusters are marked used but belong to nothing{tail}"),
            );
        }
        if fix {
            self.fat.flush()?;
        }
        Ok(report)
    }

    /// How many clusters the chain has, or -1 if it's broken.
    fn walk(&mut self, path: &str, first: u32, owned: &mut Owned, report: &mut FsckReport) -> Result<i64> {
        let count = self.geometry.cluster_count;
        let mut length = 0i64;
        let mut c = first;
        loop {
            if c < 1 || c > count {
                report.add(
                    Severity::Error,
                    format!("{path}: chain points at cluster {c}, which is outside the volume"),
                );
                return Ok(-1);
            }
            if owned.test(c) {
                report.add(
                    Severity::Error,
                    format!("{path}: cluster {c} is already used by another file or loops back"),
                );
                return Ok(-1);
            }
            owned.mark(c);
            length += 1;
            let next = self.fat.get(c)?;
            if next >= self.geometry.end_threshold() {
                return Ok(length);
            }
            if next == 0 || next == self.geometry.bad_cluster() {
                report.add(
                    Severity::Error,
                    format!("{path}: chain runs into a free or bad cluster after {c}"),
                );
                return Ok(-1);
            }
            c = next;
        }
    }
}
