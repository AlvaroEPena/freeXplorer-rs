/// One stretch of clusters that follow each other on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Extent {
    /// index of the first cluster inside the file
    pub start: u64,
    /// cluster number on the volume
    pub disk: u32,
    pub length: u64,
}

/// A file's cluster chain squashed into runs of consecutive clusters. Most files on a
/// freshly written drive are one or two runs, so this beats following the FAT per cluster.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClusterMap {
    extents: Vec<Extent>,
    count: u64,
    last_cluster: u32,
}

impl ClusterMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn last_cluster(&self) -> u32 {
        self.last_cluster
    }

    pub fn extents(&self) -> &[Extent] {
        &self.extents
    }

    pub fn append_run(&mut self, disk: u32, length: u64) {
        if length == 0 {
            return;
        }
        if let Some(last) = self.extents.last_mut()
            && last.disk as u64 + last.length == disk as u64
        {
            last.length += length;
            self.count += length;
            self.last_cluster = (disk as u64 + length - 1) as u32;
            return;
        }
        self.extents.push(Extent {
            start: self.count,
            disk,
            length,
        });
        self.count += length;
        self.last_cluster = (disk as u64 + length - 1) as u32;
    }

    pub fn append(&mut self, disk: u32) {
        self.append_run(disk, 1);
    }

    /// The cluster on disk for a cluster index in the file, and how many clusters from there
    /// (counting that one) stay together on disk.
    pub fn find(&self, index: u64) -> Option<(u32, u64)> {
        let at = self.extents.partition_point(|e| e.start + e.length <= index);
        let e = self.extents.get(at)?;
        if index < e.start {
            return None;
        }
        Some(((e.disk as u64 + (index - e.start)) as u32, e.length - (index - e.start)))
    }

    pub fn cluster_at(&self, index: u64) -> Option<u32> {
        self.find(index).map(|(disk, _)| disk)
    }

    /// Every (disk, length) run from a file cluster index onwards.
    pub fn runs_from(&self, index: u64) -> Vec<(u32, u64)> {
        self.extents
            .iter()
            .filter(|e| e.start + e.length > index)
            .map(|e| {
                let skip = index.saturating_sub(e.start);
                ((e.disk as u64 + skip) as u32, e.length - skip)
            })
            .collect()
    }

    pub fn truncate(&mut self, new_count: u64) {
        while self.extents.last().is_some_and(|e| e.start >= new_count) {
            self.extents.pop();
        }
        if let Some(e) = self.extents.last_mut()
            && e.start + e.length > new_count
        {
            e.length = new_count - e.start;
        }
        self.count = new_count;
        self.last_cluster = self.extents.last().map_or(0, |e| (e.disk as u64 + e.length - 1) as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbouring_clusters_join_into_one_run() {
        let mut map = ClusterMap::new();
        for c in [5, 6, 7, 20, 21, 40] {
            map.append(c);
        }
        assert_eq!(map.count(), 6);
        assert_eq!(map.extents().len(), 3);
        assert_eq!(map.last_cluster(), 40);
    }

    #[test]
    fn finding_a_cluster_reports_the_rest_of_its_run() {
        let mut map = ClusterMap::new();
        map.append_run(10, 4);
        map.append_run(100, 2);
        assert_eq!(map.find(0), Some((10, 4)));
        assert_eq!(map.find(3), Some((13, 1)));
        assert_eq!(map.find(4), Some((100, 2)));
        assert_eq!(map.find(5), Some((101, 1)));
        assert_eq!(map.find(6), None);
    }

    #[test]
    fn runs_from_the_middle_start_inside_a_run() {
        let mut map = ClusterMap::new();
        map.append_run(10, 4);
        map.append_run(100, 2);
        assert_eq!(map.runs_from(2), vec![(12, 2), (100, 2)]);
        assert_eq!(map.runs_from(5), vec![(101, 1)]);
        assert!(map.runs_from(6).is_empty());
    }

    #[test]
    fn truncating_cuts_a_run_and_drops_the_ones_after() {
        let mut map = ClusterMap::new();
        map.append_run(10, 4);
        map.append_run(100, 2);
        map.truncate(2);
        assert_eq!((map.count(), map.last_cluster()), (2, 11));
        map.truncate(0);
        assert_eq!((map.count(), map.last_cluster()), (0, 0));
        assert!(map.find(0).is_none());
    }
}
