//! Reading what macOS's `diskutil` says about the disks. It prints property lists, and the
//! program that runs it is passed in, so the tests give it saved answers instead.

use std::collections::HashSet;
use std::io::Cursor;

use plist::{Dictionary, Value};

use crate::DiskInfo;

/// Runs `diskutil` with these arguments and gives back what it printed, if it worked.
pub type Run<'a> = &'a dyn Fn(&[&str]) -> Option<Vec<u8>>;

fn parse(bytes: Vec<u8>) -> Option<Dictionary> {
    Value::from_reader(Cursor::new(bytes)).ok()?.into_dictionary()
}

fn text<'a>(dict: &'a Dictionary, key: &str) -> Option<&'a str> {
    dict.get(key)?.as_string()
}

fn number(dict: &Dictionary, key: &str) -> Option<u64> {
    let value = dict.get(key)?;
    value
        .as_unsigned_integer()
        .or_else(|| value.as_signed_integer().map(|n| n.max(0) as u64))
}

fn flag(dict: &Dictionary, key: &str) -> bool {
    dict.get(key).and_then(Value::as_boolean).unwrap_or(false)
}

/// "disk0s2" is on "disk0".
pub fn whole_disk(identifier: &str) -> String {
    match identifier.strip_prefix("disk") {
        Some(rest) => {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            format!("disk{digits}")
        }
        None => identifier.to_string(),
    }
}

fn mount_points(entry: &Dictionary) -> Vec<String> {
    ["Partitions", "APFSVolumes"]
        .iter()
        .filter_map(|key| entry.get(key)?.as_array())
        .flatten()
        .filter_map(|v| text(v.as_dictionary()?, "MountPoint"))
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn physical_stores(entry: &Dictionary) -> Vec<String> {
    entry
        .get("APFSPhysicalStores")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| text(s.as_dictionary()?, "DeviceIdentifier"))
        .map(whole_disk)
        .collect()
}

pub fn list(run: Run) -> Vec<DiskInfo> {
    let Some(listing) = run(&["list", "-plist"]).and_then(parse) else {
        return Vec::new();
    };
    let entries: Vec<&Dictionary> = listing
        .get("AllDisksAndPartitions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_dictionary)
        .collect();

    // Which physical disks have something mounted from them. An APFS container is a disk of its own
    // that only exists in software, what is mounted from it lives on the physical disks under it.
    let mut busy: HashSet<String> = HashSet::new();
    let mut system: HashSet<String> = HashSet::new();
    for entry in &entries {
        let Some(id) = text(entry, "DeviceIdentifier") else {
            continue;
        };
        let mounts = mount_points(entry);
        if mounts.is_empty() {
            continue;
        }
        let is_system = mounts.iter().any(|m| m == "/" || m.starts_with("/System/Volumes/Data"));
        let stores = physical_stores(entry);
        let disks = if stores.is_empty() {
            vec![whole_disk(id)]
        } else {
            stores
        };
        for disk in disks {
            if is_system {
                system.insert(disk.clone());
            }
            busy.insert(disk);
        }
    }

    let mut disks = Vec::new();
    for entry in &entries {
        let Some(id) = text(entry, "DeviceIdentifier") else {
            continue;
        };
        if !physical_stores(entry).is_empty() {
            continue; // a container, not a disk
        }
        let Some(info) = run(&["info", "-plist", id]).and_then(parse) else {
            continue;
        };
        let bus = text(&info, "BusProtocol").unwrap_or("Unknown");
        if text(&info, "VirtualOrPhysical") == Some("Virtual") || bus == "Disk Image" {
            continue;
        }
        let length = number(entry, "Size")
            .or_else(|| number(&info, "TotalSize"))
            .or_else(|| number(&info, "Size"))
            .unwrap_or(0);
        if length == 0 {
            continue;
        }
        disks.push(DiskInfo {
            id: id.to_string(),
            path: format!("/dev/r{id}"),
            length,
            sector_size: number(&info, "DeviceBlockSize").filter(|&s| s > 0).unwrap_or(512) as u32,
            model: text(&info, "MediaName").unwrap_or("").trim().to_string(),
            serial: String::new(),
            bus: bus.to_string(),
            removable: flag(&info, "RemovableMediaOrExternalDevice")
                || flag(&info, "RemovableMedia")
                || flag(&info, "Removable"),
            has_volumes: busy.contains(id),
            is_system: system.contains(id),
        });
    }
    disks
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
 <key>AllDisksAndPartitions</key><array>
  <dict>
   <key>DeviceIdentifier</key><string>disk0</string>
   <key>Size</key><integer>500277792768</integer>
   <key>Partitions</key><array>
     <dict><key>DeviceIdentifier</key><string>disk0s1</string><key>Content</key><string>Apple_APFS_ISC</string></dict>
     <dict><key>DeviceIdentifier</key><string>disk0s2</string><key>Content</key><string>Apple_APFS</string></dict>
   </array>
  </dict>
  <dict>
   <key>DeviceIdentifier</key><string>disk3</string>
   <key>Size</key><integer>494384795648</integer>
   <key>APFSPhysicalStores</key><array><dict><key>DeviceIdentifier</key><string>disk0s2</string></dict></array>
   <key>APFSVolumes</key><array>
     <dict><key>DeviceIdentifier</key><string>disk3s1s1</string><key>MountPoint</key><string>/</string></dict>
     <dict><key>DeviceIdentifier</key><string>disk3s5</string><key>MountPoint</key><string>/System/Volumes/Data</string></dict>
   </array>
  </dict>
  <dict>
   <key>DeviceIdentifier</key><string>disk4</string>
   <key>Size</key><integer>256060514304</integer>
   <key>Partitions</key><array>
     <dict><key>DeviceIdentifier</key><string>disk4s1</string><key>Content</key><string>Microsoft Basic Data</string><key>MountPoint</key><string>/Volumes/STICK</string></dict>
   </array>
  </dict>
  <dict>
   <key>DeviceIdentifier</key><string>disk5</string>
   <key>Size</key><integer>250059350016</integer>
   <key>Partitions</key><array/>
  </dict>
  <dict>
   <key>DeviceIdentifier</key><string>disk6</string>
   <key>Size</key><integer>1000000000</integer>
  </dict>
 </array>
</dict></plist>"#;

    fn info(media: &str, bus: &str, block: u64, virtual_or_physical: &str, external: bool) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
 <key>MediaName</key><string>{media}</string>
 <key>BusProtocol</key><string>{bus}</string>
 <key>DeviceBlockSize</key><integer>{block}</integer>
 <key>VirtualOrPhysical</key><string>{virtual_or_physical}</string>
 <key>RemovableMediaOrExternalDevice</key><{ext}/>
</dict></plist>"#,
            ext = if external { "true" } else { "false" }
        )
    }

    fn run(args: &[&str]) -> Option<Vec<u8>> {
        match args {
            ["list", "-plist"] => Some(LIST.as_bytes().to_vec()),
            ["info", "-plist", "disk0"] => {
                Some(info("APPLE SSD AP0512Q", "Apple Fabric", 4096, "Physical", false).into_bytes())
            }
            ["info", "-plist", "disk4"] => Some(info("USB DISK 3.0", "USB", 512, "Physical", true).into_bytes()),
            ["info", "-plist", "disk5"] => Some(info("SABRENT ", "USB", 512, "Physical", true).into_bytes()),
            ["info", "-plist", "disk6"] => {
                Some(info("Apple Disk Image", "Disk Image", 512, "Virtual", false).into_bytes())
            }
            _ => None,
        }
    }

    #[test]
    fn partitions_are_on_their_whole_disk() {
        assert_eq!(whole_disk("disk0s2"), "disk0");
        assert_eq!(whole_disk("disk12"), "disk12");
        assert_eq!(whole_disk("disk3s1s1"), "disk3");
    }

    #[test]
    fn the_disks_come_out_with_what_is_mounted_from_them() {
        let disks = list(&run);
        let ids: Vec<&str> = disks.iter().map(|d| d.id.as_str()).collect();
        // the APFS container disk3 and the disk image are not disks to open
        assert_eq!(ids, ["disk0", "disk4", "disk5"]);

        let by = |id: &str| disks.iter().find(|d| d.id == id).unwrap();
        // what is mounted from the container is mounted from the physical disk underneath it
        assert!(by("disk0").is_system && by("disk0").has_volumes);
        assert!(by("disk4").has_volumes && !by("disk4").is_system);
        assert!(!by("disk5").has_volumes && !by("disk5").is_system);

        assert_eq!(by("disk0").sector_size, 4096);
        assert_eq!(by("disk5").model, "SABRENT");
        assert_eq!(by("disk5").path, "/dev/rdisk5");
        assert_eq!(by("disk5").length, 250_059_350_016);
        assert!(by("disk5").removable && !by("disk0").removable);
    }

    #[test]
    fn nothing_comes_out_when_diskutil_fails() {
        assert!(list(&|_| None).is_empty());
        assert!(list(&|_| Some(b"not a plist".to_vec())).is_empty());
    }
}
