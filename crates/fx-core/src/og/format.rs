//! Writing an original Xbox drive: the table in sector zero, then one FATX volume per partition.
//!
//! This is the mirror of `scanner.rs`. The reader takes a drive apart and this puts one together,
//! and both go through `layout`, so the layout a format writes is the layout the reader finds.
//!
//! What it writes is a *quick* format: the header, an allocation table zeroed all the way to its
//! own end, and a root directory cleared to `0xFF`. The data area is left alone and unreferenced.
//! Zeroing the table is the part neither of the drives measured here got, and the reason they
//! report millions of clusters that belong to no file.

use std::sync::Arc;

use super::layout::{self, Layout};
use super::mbr::{self, Entry, MAX_LBA, SECTOR_SIZE};
use crate::device::{Device, SliceDevice};
use crate::error::{Error, ErrorKind, Result};
use crate::fatx::geometry::MAX_DATA_CLUSTERS_OG;
use crate::fatx::{self, FatxVolume, Flavor, FormatOptions, Geometry};
use crate::partition::PartitionSpec;

/// What a format is asked for.
#[derive(Debug, Clone)]
pub struct Options {
    pub layout: Layout,
    /// 0 means one is picked per partition from its size, which reproduces the 16 KB the consoles
    /// use on a 250 GB drive and the 64 KB they use on a 2 TB one.
    pub sectors_per_cluster: u32,
    /// Written onto E, the partition the dashboard shows. Ignored when empty.
    pub label: Option<String>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            layout: Layout::Auto,
            sectors_per_cluster: 0,
            label: None,
        }
    }
}

/// One partition a format will write, with the geometry it will get.
#[derive(Debug, Clone)]
pub struct Step {
    pub spec: PartitionSpec,
    pub sectors_per_cluster: u32,
    pub geometry: Geometry,
}

/// The partitions a format will write and the geometry each one gets, worked out before anything
/// is written.
///
/// This is what a dry run prints: the same numbers the write uses, so the table a person approves
/// is the table that lands.
pub fn steps(length: u64, options: &Options) -> Result<Vec<Step>> {
    steps_for(&layout::plan(length, options.layout)?, options)
}

/// The geometry each of these partitions gets, for a format that already knows where they are.
///
/// Reformatting a partition in place goes through here: the drive's own table says how big the
/// partition is, so there is no layout to plan, but the cluster size and the limits are the same
/// ones a fresh layout gets.
pub fn steps_for(specs: &[PartitionSpec], options: &Options) -> Result<Vec<Step>> {
    let mut steps = Vec::with_capacity(specs.len());
    for spec in specs {
        if spec.length == 0 {
            continue;
        }
        if spec.offset / SECTOR_SIZE > MAX_LBA {
            return Err(Error::invalid(format!(
                "{} starts past what a table can address",
                spec.name
            )));
        }
        let spc = if options.sectors_per_cluster != 0 {
            options.sectors_per_cluster
        } else {
            Geometry::pick_sectors_per_cluster_og(spec.length)
        };
        let geometry = Geometry::compute(spec.length, spc)?;
        if !Geometry::og_holds(spec.length, spc) {
            return Err(Error::fatx(
                ErrorKind::InvalidOperation,
                format!(
                    "{} would need {} clusters, more than the {MAX_DATA_CLUSTERS_OG} a console \
                     reads; that partition is too large for one volume",
                    spec.name, geometry.cluster_count
                ),
            ));
        }
        steps.push(Step {
            spec: spec.clone(),
            sectors_per_cluster: spc,
            geometry,
        });
    }
    Ok(steps)
}

/// Writes the table and then every volume.
///
/// The table goes first: a format that fails half way then leaves a drive that says what was
/// intended, instead of a set of volumes that nothing describes.
pub fn drive(
    device: &Device,
    steps: &[Step],
    options: &Options,
    progress: impl FnMut(&Step, f64),
) -> Result<()> {
    let entries = table_entries(steps.iter().map(|s| &s.spec));
    mbr::write(device.as_ref(), &entries)?;
    volumes(device, steps, options, progress)
}

/// Writes the volumes of a plan and leaves sector zero alone.
///
/// This is reformatting a partition in place: the drive's table already describes it, and rewriting
/// the table would be a second, separate promise about the drive.
pub fn volumes(
    device: &Device,
    steps: &[Step],
    options: &Options,
    mut progress: impl FnMut(&Step, f64),
) -> Result<()> {
    for step in steps {
        let format = FormatOptions {
            flavor: Flavor::Xbox,
            sectors_per_cluster: step.sectors_per_cluster,
            volume_id: None,
            label: options
                .label
                .as_deref()
                .filter(|l| !l.is_empty() && step.spec.id == "e")
                .map(str::to_string),
        };
        fatx::format(device, step.spec.offset, step.spec.length, &format, |p| {
            progress(step, p)
        })?;
    }
    device.flush()?;
    Ok(())
}

/// The entries a table gets for these partitions, in the slot order the drives here use.
///
/// Nothing reads the order back — the parser sorts by where a partition starts — but every table
/// measured so far is E, C, X, Y, Z, then the extended partitions, so ours is too.
fn table_entries<'a>(specs: impl IntoIterator<Item = &'a PartitionSpec>) -> Vec<Entry> {
    let mut sorted: Vec<&PartitionSpec> = specs.into_iter().collect();
    sorted.sort_by_key(|s| layout::slot_order(s.id));
    sorted
        .into_iter()
        .map(|s| {
            Entry::new(
                layout::table_name(s.id),
                (s.offset / SECTOR_SIZE) as u32,
                (s.length / SECTOR_SIZE) as u32,
            )
        })
        .collect()
}

/// Reads back what a format wrote, and refuses to call it done if any of it disagrees.
///
/// Every check here is a read. The table has to parse and name the same starts; each volume has to
/// open as FATX, hold nothing but its own root and the label file, and come out of a check clean
/// with exactly the free space the geometry implies. That free figure is the number the console
/// shows, so it is the one worth being wrong about least.
pub fn verify(device: &Device, steps: &[Step], options: &Options) -> Result<()> {
    let table = mbr::read(device.as_ref())?
        .ok_or_else(|| Error::invalid("the table just written doesn't read back as a table"))?;
    for step in steps {
        let start = (step.spec.offset / SECTOR_SIZE) as u32;
        let size = step.spec.length / SECTOR_SIZE;
        if !table
            .entries
            .iter()
            .any(|e| e.start_lba == start && e.size_lba as u64 == size)
        {
            return Err(Error::invalid(format!(
                "{} isn't in the table the way it was written",
                step.spec.name
            )));
        }
        let slice: Device = Arc::new(SliceDevice::new(
            device.clone(),
            step.spec.offset,
            step.spec.length,
            false,
        )?);
        let mut volume = FatxVolume::open(slice, false)?;
        if volume.flavor() != Flavor::Xbox {
            return Err(Error::invalid(format!("{} isn't a FATX volume", step.spec.name)));
        }
        let report = volume.check(false, |_| {})?;
        if !report.is_clean() {
            let first = report
                .issues
                .first()
                .map(|i| i.message.as_str())
                .unwrap_or("no detail");
            return Err(Error::invalid(format!(
                "{} doesn't check out after the format: {first}",
                step.spec.name
            )));
        }
        // a label is a file in the root, so E is the one volume that isn't empty
        let files = u64::from(
            step.spec.id == "e" && options.label.as_deref().is_some_and(|l| !l.is_empty()),
        );
        let count = step.geometry.cluster_count as u64;
        if report.directories != 1 || report.files != files {
            return Err(Error::invalid(format!(
                "{} has {} folders and {} files in it, and it was just formatted",
                step.spec.name, report.directories, report.files
            )));
        }
        // the root, and one cluster for the label
        let used = 1 + files;
        if report.used_clusters != used || report.free_clusters != count - used {
            return Err(Error::invalid(format!(
                "{} says {} of {} clusters are in use, and only its root should be",
                step.spec.name, report.used_clusters, count
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{BlockDevice, MemoryDevice};
    use crate::og::{from_table, scan};
    use crate::scan::DriveKind;
    use crate::testing::{assert_clean, random_bytes};

    const DRIVE_250GB: u64 = 250_000_000_000;
    const DRIVE_2TB: u64 = 2_000_398_934_016;
    /// Small enough to write in a test in a second, large enough for all five partitions and an F.
    const DRIVE_10GB: u64 = 10_000_000_000;

    fn device_of(length: u64) -> (Arc<MemoryDevice>, Device) {
        let device = Arc::new(MemoryDevice::new(length));
        let shared: Device = device.clone();
        (device, shared)
    }

    /// The layout a format writes has to be the one the drive measured here carries: the same
    /// starts, the same sizes, and the same FAT, so that files land where the console put them.
    #[test]
    fn the_plan_reproduces_the_250_gb_drive_we_measured() {
        let specs = layout::plan(DRIVE_250GB, Layout::Auto).unwrap();
        let ids: Vec<&str> = specs.iter().map(|s| s.id).collect();
        assert_eq!(ids, ["x", "y", "z", "c", "e", "f"], "250 GB fits in one F");

        let f = layout::find(&specs, "f").unwrap();
        assert_eq!(f.offset, 0xEE8AB0 * 512);
        assert_eq!(f.length / 512, 0x1C2C09F2, "the size the drive's own table carries");
        assert_eq!(f.end(), DRIVE_250GB, "F ends flush with the drive");

        let steps = steps(DRIVE_250GB, &Options::default()).unwrap();
        let cluster_sizes: Vec<u32> = steps.iter().map(|s| s.sectors_per_cluster).collect();
        assert_eq!(cluster_sizes, [0x20; 6], "every partition takes 16 KB clusters");

        let e = &steps[4].geometry;
        assert_eq!(e.fat_size, 1_253_376);
        assert_eq!(e.data_offset, 0x133000, "E's data starts where the drive's does");

        let f = &steps[5].geometry;
        assert_eq!(f.fat_size, 59_084_800, "the FAT the drive has");
        assert_eq!(f.data_offset, 0x385A000, "and the offset its files start at");
        // the console counts 14,770,256 clusters, but the last 3,607 of those would start past the
        // end of the partition, so the reader stops at the ones that fit
        assert_eq!(f.cluster_count, 14_766_649);
    }

    /// A 2 TB drive cannot be one volume: at the largest cluster a console reads it needs thirty
    /// million clusters and the cap is sixteen. Splitting its extended space evenly gives exactly
    /// the two partitions on the drive here.
    #[test]
    fn a_2_tb_drive_is_split_into_f_and_g_at_64_kb() {
        let steps = steps(DRIVE_2TB, &Options::default()).unwrap();
        let f = steps.iter().find(|s| s.spec.id == "f").unwrap();
        let g = steps.iter().find(|s| s.spec.id == "g").unwrap();
        assert_eq!(f.spec.length / 512, 0x73F8FF00, "the size on the 2 TB drive");
        assert_eq!(g.spec.length / 512, 0x73F8FF00);
        assert_eq!(g.spec.offset, f.spec.end(), "G starts where F stops");
        assert_eq!(g.spec.end(), DRIVE_2TB, "and runs to the end of the drive");
        assert_eq!(f.sectors_per_cluster, 0x80, "64 KB, which is what the 2 TB drive has");
        assert_eq!(g.sectors_per_cluster, 0x80);
        assert!(Geometry::og_holds(f.spec.length, f.sectors_per_cluster));
        assert!(Geometry::og_holds(g.spec.length, g.sectors_per_cluster));
    }

    /// Asking for one F anyway is allowed, and refused with a reason when no console could read it.
    #[test]
    fn one_f_is_refused_when_no_console_could_read_it() {
        let options = Options {
            layout: Layout::FAll,
            ..Options::default()
        };
        let err = steps(DRIVE_2TB, &options).unwrap_err();
        assert!(err.to_string().contains("one F"), "{err}");
    }

    /// Sector zero of the 250 GB drive, parsed and written back, has to come out the same: the
    /// magic, the slot order, the padding, and the four zero bytes at the end of every used slot.
    ///
    /// The one place ours differs is slot six, where the drive names an "XBOX G" it never made.
    /// That name is decoration and is not copied, but every blank slot after it is identical.
    #[test]
    fn the_writer_reproduces_a_real_drives_table() {
        let original = crate::og::mbr::tests::sector0();
        let table = mbr::Table::parse(&original).unwrap();
        let specs = from_table(&table, DRIVE_250GB);
        let written = mbr::table_bytes(&table_entries(specs.iter()));

        let named = mbr::TABLE_PREFIX + 6 * mbr::ENTRY_SIZE;
        assert_eq!(&written[..named], &original[..named], "the six partitions this drive has");

        for slot in 6..mbr::ENTRY_COUNT {
            let at = mbr::TABLE_PREFIX + slot * mbr::ENTRY_SIZE;
            assert_eq!(&written[at..at + 16], &[0x20; 16], "a blank slot is sixteen spaces");
            assert_eq!(&written[at + 16..at + 32], &[0u8; 16], "and then sixteen zeroes");
        }
        let blank = mbr::TABLE_PREFIX + 7 * mbr::ENTRY_SIZE;
        assert_eq!(&written[blank..], &original[blank..mbr::TABLE_BYTES], "the blank slots the drive has");
    }

    /// The boot loader starts at 0x200 and is not ours to write, at either end of a format.
    #[test]
    fn a_format_never_touches_the_boot_loader() {
        let (device, shared) = device_of(DRIVE_10GB);
        let loader = random_bytes(0x400, 7);
        device.write_at(0x200, &loader).unwrap();

        let steps = steps(DRIVE_10GB, &Options::default()).unwrap();
        drive(&shared, &steps, &Options::default(), |_, _| {}).unwrap();

        let mut after = vec![0u8; loader.len()];
        device.read_at(0x200, &mut after).unwrap();
        assert_eq!(after, loader, "the boot loader must come through a format unchanged");
    }

    /// The whole thing: the table, every volume, the read-back, and then the reader the window
    /// and the command line both use.
    #[test]
    fn a_formatted_drive_reads_back_and_every_volume_is_clean() {
        let (_device, shared) = device_of(DRIVE_10GB);
        let steps = steps(DRIVE_10GB, &Options::default()).unwrap();
        let options = Options {
            label: Some("XBOX DATA".into()),
            ..Options::default()
        };

        let mut last = 0.0;
        drive(&shared, &steps, &options, |_, p| last = p).unwrap();
        assert!(last > 0.0);
        verify(&shared, &steps, &options).unwrap();

        // the reader sees the drive we meant to make, not something that merely opens
        let scan = scan(&*shared).unwrap().expect("a formatted drive is an original Xbox drive");
        assert_eq!(scan.kind, DriveKind::OgDrive);
        assert_eq!(scan.partitions.len(), 6);
        assert!(scan.partitions.iter().all(|p| p.has_signature));

        // a file written afterwards reads back byte-identical, which is what proves the
        // allocation table and the data offset agree
        let e = steps.iter().find(|s| s.spec.id == "e").unwrap();
        let slice: Device = Arc::new(
            SliceDevice::new(shared.clone(), e.spec.offset, e.spec.length, true).unwrap(),
        );
        let mut volume = FatxVolume::open(slice, true).unwrap();
        assert_eq!(volume.label().unwrap().as_deref(), Some("XBOX DATA"));
        let root = volume.root();
        let data = random_bytes(e.geometry.cluster_size as usize + 1, 3);
        let file = volume.create_file(root, "probe.bin").unwrap();
        volume.write(file, 0, &data).unwrap();
        volume.flush().unwrap();
        let mut back = vec![0u8; data.len()];
        assert_eq!(volume.read(file, 0, &mut back).unwrap(), data.len());
        assert_eq!(back, data);
        assert_clean(&mut volume);

        // and the volumes nobody wrote to are still empty after all of that
        let untouched: Vec<Step> = steps.iter().filter(|s| s.spec.id != "e").cloned().collect();
        verify(&shared, &untouched, &Options::default()).unwrap();
    }

    #[test]
    fn a_drive_too_small_for_the_five_partitions_is_refused() {
        let err = layout::plan(4 << 30, Layout::Auto).unwrap_err();
        assert!(err.to_string().contains("too small"), "{err}");
    }

    #[test]
    fn a_cluster_size_that_cannot_cover_the_length_is_refused() {
        let options = Options {
            layout: Layout::Auto,
            sectors_per_cluster: 1,
            label: None,
        };
        let err = steps(DRIVE_2TB, &options).unwrap_err();
        assert!(err.to_string().contains("clusters"), "{err}");
    }
}
