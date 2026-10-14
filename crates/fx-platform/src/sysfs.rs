//! What Linux says about its disks. /sys and /proc are plain files, so this reads them from any
//! folder, which is how the tests run it on a made up copy.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::DiskInfo;

pub struct Roots {
    pub sys: PathBuf,
    pub proc: PathBuf,
    pub dev: PathBuf,
}

impl Roots {
    pub fn real() -> Roots {
        Roots {
            sys: "/sys".into(),
            proc: "/proc".into(),
            dev: "/dev".into(),
        }
    }
}

/// Virtual and optical devices that aren't somewhere a console drive could be.
pub fn is_ignored(name: &str) -> bool {
    ["loop", "ram", "zram", "sr", "fd", "dm-", "md", "nbd", "zd", "rbd"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn number(path: &Path) -> Option<u64> {
    read(path)?.parse().ok()
}

/// "USB", "SATA", "NVMe"... from the device's name and, when there is one, where sysfs says it sits.
pub fn bus_of(name: &str, device_path: Option<&str>) -> &'static str {
    if name.starts_with("nvme") {
        return "NVMe";
    }
    if name.starts_with("mmcblk") {
        return "SD";
    }
    if name.starts_with("vd") || name.starts_with("xvd") {
        return "Virtual";
    }
    match device_path {
        Some(p) if p.contains("/usb") => "USB",
        Some(p) if p.contains("/ata") => "SATA",
        Some(p) if p.contains("/virtio") => "Virtual",
        Some(p) if p.contains("/mmc") => "SD",
        _ if name.starts_with("hd") => "ATA",
        _ => "SCSI",
    }
}

/// The disk a partition belongs to: sdb1 is on sdb, nvme0n1p2 on nvme0n1.
pub fn disk_of_partition(name: &str) -> String {
    let digits = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if digits.len() == name.len() {
        return name.to_string();
    }
    // disks whose own name ends in a digit get a "p" before the partition number
    if let Some(base) = digits.strip_suffix('p')
        && base.ends_with(|c: char| c.is_ascii_digit())
    {
        return base.to_string();
    }
    // those same kinds of names, without a "p", are the whole disk
    if ["nvme", "mmcblk", "loop", "md", "dm-", "zram", "nbd"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return name.to_string();
    }
    digits.to_string()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Mount {
    pub source: String,
    pub mount_point: String,
    pub fs_type: String,
}

fn unescape(field: &str) -> String {
    // spaces and the like come as \040
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let code = (bytes[i] == b'\\' && i + 4 <= bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 4]).ok())
            .flatten()
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match code {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The lines of /proc/self/mountinfo.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let mount_point = before.split_whitespace().nth(4)?;
            let mut rest = after.split_whitespace();
            let fs_type = rest.next()?;
            let source = rest.next()?;
            Some(Mount {
                source: unescape(source),
                mount_point: unescape(mount_point),
                fs_type: fs_type.to_string(),
            })
        })
        .collect()
}

pub fn mounts(roots: &Roots) -> Vec<Mount> {
    read(&roots.proc.join("self").join("mountinfo"))
        .map(|t| parse_mountinfo(&t))
        .unwrap_or_default()
}

/// The names of the whole disks that a device like /dev/mapper/root or /dev/sda2 sits on.
fn disks_under(roots: &Roots, name: &str, seen: &mut HashSet<String>) -> HashSet<String> {
    let mut found = HashSet::new();
    if !seen.insert(name.to_string()) {
        return found;
    }
    if roots.sys.join("block").join(name).is_dir() {
        let slaves = roots.sys.join("block").join(name).join("slaves");
        let mut had_slaves = false;
        if let Ok(entries) = fs::read_dir(slaves) {
            for entry in entries.flatten() {
                had_slaves = true;
                found.extend(disks_under(roots, &entry.file_name().to_string_lossy(), seen));
            }
        }
        if !had_slaves {
            found.insert(name.to_string());
        }
    } else {
        let disk = disk_of_partition(name);
        if disk != name && roots.sys.join("block").join(&disk).is_dir() {
            found.extend(disks_under(roots, &disk, seen));
        }
    }
    found
}

/// What device name /dev/whatever really is, following links like /dev/mapper/vg-root.
fn device_name(roots: &Roots, source: &str) -> Option<String> {
    let path = Path::new(source);
    if !path.starts_with(&roots.dev) {
        return None;
    }
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolved.file_name().map(|n| n.to_string_lossy().into_owned())
}

pub struct InUse {
    /// disks with a file system mounted from them, or swap on them
    pub busy: HashSet<String>,
    /// the disks the root file system is on
    pub system: HashSet<String>,
}

pub fn in_use(roots: &Roots) -> InUse {
    let mut busy = HashSet::new();
    let mut system = HashSet::new();
    for mount in mounts(roots) {
        let Some(name) = device_name(roots, &mount.source) else {
            continue;
        };
        let disks = disks_under(roots, &name, &mut HashSet::new());
        if mount.mount_point == "/" {
            system.extend(disks.iter().cloned());
        }
        busy.extend(disks);
    }
    // swap is a use too, without being mounted
    if let Some(swaps) = read(&roots.proc.join("swaps")) {
        for line in swaps.lines().skip(1) {
            if let Some(source) = line.split_whitespace().next()
                && let Some(name) = device_name(roots, source)
            {
                busy.extend(disks_under(roots, &name, &mut HashSet::new()));
            }
        }
    }
    InUse { busy, system }
}

pub fn list(roots: &Roots) -> Vec<DiskInfo> {
    let usage = in_use(roots);
    let mut names: Vec<String> = fs::read_dir(roots.sys.join("block"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !is_ignored(n))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| natural(n));

    let mut disks = Vec::new();
    for name in names {
        let base = roots.sys.join("block").join(&name);
        let Some(sectors) = number(&base.join("size")).filter(|&s| s > 0) else {
            continue;
        };
        let vendor = read(&base.join("device").join("vendor")).unwrap_or_default();
        let product = read(&base.join("device").join("model")).unwrap_or_default();
        let device_path = fs::read_link(&base).ok().map(|p| p.to_string_lossy().into_owned());
        disks.push(DiskInfo {
            path: roots.dev.join(&name).to_string_lossy().into_owned(),
            length: sectors * 512,
            sector_size: number(&base.join("queue").join("logical_block_size")).unwrap_or(512) as u32,
            model: format!("{vendor} {product}").trim().to_string(),
            serial: read(&base.join("device").join("serial")).unwrap_or_default(),
            bus: bus_of(&name, device_path.as_deref()).to_string(),
            removable: read(&base.join("removable")).as_deref() == Some("1"),
            has_volumes: usage.busy.contains(&name),
            is_system: usage.system.contains(&name),
            id: name,
        });
    }
    disks
}

/// sda before sdb, and sdb2 before sdb10 and nvme0n1 before nvme1n1
fn natural(name: &str) -> (String, u64) {
    let prefix = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits: u64 = name[prefix.len()..].parse().unwrap_or(0);
    (prefix.to_string(), digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: PathBuf, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn disk(roots: &Roots, name: &str, sectors: u64, vendor: &str, model: &str, removable: bool) {
        let base = roots.sys.join("block").join(name);
        write(base.join("size"), &format!("{sectors}\n"));
        write(base.join("removable"), if removable { "1\n" } else { "0\n" });
        write(base.join("queue").join("logical_block_size"), "512\n");
        write(base.join("device").join("vendor"), &format!("{vendor}  \n"));
        write(base.join("device").join("model"), &format!("{model}\n"));
    }

    fn fake() -> (tempfile::TempDir, Roots) {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots {
            sys: dir.path().join("sys"),
            proc: dir.path().join("proc"),
            dev: dir.path().join("dev"),
        };
        fs::create_dir_all(&roots.dev).unwrap();
        (dir, roots)
    }

    #[test]
    fn partitions_belong_to_their_disk() {
        for (part, disk) in [
            ("sdb1", "sdb"),
            ("sdb", "sdb"),
            ("sda12", "sda"),
            ("nvme0n1p2", "nvme0n1"),
            ("nvme0n1", "nvme0n1"),
            ("mmcblk0p1", "mmcblk0"),
            ("vda3", "vda"),
        ] {
            assert_eq!(disk_of_partition(part), disk, "{part}");
        }
    }

    #[test]
    fn virtual_devices_are_ignored() {
        for name in ["loop0", "ram1", "zram0", "sr0", "dm-0", "md127", "nbd0"] {
            assert!(is_ignored(name), "{name}");
        }
        for name in ["sda", "nvme0n1", "mmcblk0", "vda"] {
            assert!(!is_ignored(name), "{name}");
        }
    }

    #[test]
    fn the_bus_comes_from_the_name_or_where_it_sits() {
        assert_eq!(bus_of("nvme0n1", None), "NVMe");
        assert_eq!(bus_of("mmcblk0", None), "SD");
        assert_eq!(bus_of("vda", None), "Virtual");
        assert_eq!(
            bus_of(
                "sdb",
                Some("../devices/pci0000:00/0000:00:14.0/usb1/1-1/host4/target4:0:0/4:0:0:0/block/sdb")
            ),
            "USB"
        );
        assert_eq!(
            bus_of(
                "sda",
                Some("../devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda")
            ),
            "SATA"
        );
        assert_eq!(bus_of("sdc", None), "SCSI");
    }

    #[test]
    fn mountinfo_lines_are_read_with_their_escapes() {
        let text = "\
36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw,errors=continue
40 30 8:17 / /media/My\\040Drive rw - vfat /dev/sdb1 rw
41 30 0:25 / /run rw - tmpfs tmpfs rw
";
        let mounts = parse_mountinfo(text);
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[0].source, "/dev/root");
        assert_eq!(mounts[0].mount_point, "/mnt2");
        assert_eq!(mounts[1].mount_point, "/media/My Drive");
        assert_eq!(mounts[1].fs_type, "vfat");
        assert_eq!(mounts[2].source, "tmpfs");
    }

    #[test]
    fn a_disk_is_described_from_its_files() {
        let (_dir, roots) = fake();
        disk(&roots, "sda", 976_773_168, "ATA", "ST2000DM001", false);
        disk(&roots, "sdb", 468_862_128, "SABRENT", "Rocket", true);
        disk(&roots, "loop0", 100, "", "", false);
        disk(&roots, "sdc", 0, "", "empty reader", true);

        let disks = list(&roots);
        let ids: Vec<&str> = disks.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["sda", "sdb"], "loop devices and empty readers are left out");
        assert_eq!(disks[0].length, 976_773_168 * 512);
        assert_eq!(disks[0].model, "ATA ST2000DM001");
        assert!(disks[1].removable && !disks[0].removable);
        assert_eq!(disks[1].path, roots.dev.join("sdb").to_string_lossy());
    }

    #[test]
    fn the_disk_with_root_is_the_system_disk_and_mounted_ones_are_busy() {
        let (_dir, roots) = fake();
        disk(&roots, "sda", 1000, "", "System", false);
        disk(&roots, "sdb", 1000, "", "Data", false);
        disk(&roots, "sdc", 1000, "", "Xbox", true);
        // partitions are folders too, which is how disks_under knows sdb1 isn't a disk
        for part in ["sda1", "sda2", "sdb1"] {
            fs::create_dir_all(roots.sys.join("class").join("block").join(part)).unwrap();
        }
        for part in ["sda2", "sdb1"] {
            fs::write(roots.dev.join(part), b"").unwrap();
        }
        write(
            roots.proc.join("self").join("mountinfo"),
            &format!(
                "20 1 8:2 / / rw - ext4 {} rw\n21 1 8:17 / /data rw - ext4 {} rw\n22 1 0:5 / /proc rw - proc proc rw\n",
                roots.dev.join("sda2").display(),
                roots.dev.join("sdb1").display()
            ),
        );

        let disks = list(&roots);
        let by = |id: &str| disks.iter().find(|d| d.id == id).unwrap();
        assert!(by("sda").is_system && by("sda").has_volumes);
        assert!(!by("sdb").is_system && by("sdb").has_volumes);
        assert!(!by("sdc").is_system && !by("sdc").has_volumes);
    }

    #[test]
    fn a_volume_group_counts_as_the_disks_under_it() {
        let (_dir, roots) = fake();
        disk(&roots, "sda", 1000, "", "One", false);
        disk(&roots, "sdb", 1000, "", "Two", false);
        let dm = roots.sys.join("block").join("dm-0");
        fs::create_dir_all(dm.join("slaves").join("sda")).unwrap();
        fs::create_dir_all(dm.join("slaves").join("sdb")).unwrap();
        fs::write(roots.dev.join("dm-0"), b"").unwrap();
        write(
            roots.proc.join("self").join("mountinfo"),
            &format!("20 1 254:0 / / rw - ext4 {} rw\n", roots.dev.join("dm-0").display()),
        );
        let disks = list(&roots);
        assert!(disks.iter().all(|d| d.is_system && d.has_volumes));
    }

    #[test]
    fn swap_makes_a_disk_busy() {
        let (_dir, roots) = fake();
        disk(&roots, "sda", 1000, "", "One", false);
        fs::write(roots.dev.join("sda"), b"").unwrap();
        write(
            roots.proc.join("swaps"),
            &format!(
                "Filename Type Size Used Priority\n{} partition 1000 0 -2\n",
                roots.dev.join("sda").display()
            ),
        );
        let disks = list(&roots);
        assert!(disks[0].has_volumes && !disks[0].is_system);
    }
}
