//! What the name of a file system says about whether a backup fits on it.

#[derive(Debug, PartialEq, Eq)]
pub struct Traits {
    /// holes in a file cost nothing here
    pub sparse: bool,
    /// the biggest file it takes, if there's a limit
    pub max_file_size: Option<u64>,
    pub network: bool,
}

pub fn classify(fs_type: &str) -> Traits {
    let name = fs_type.to_ascii_lowercase();
    let is = |names: &[&str]| names.contains(&name.as_str());
    Traits {
        sparse: is(&[
            "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "zfs", "tmpfs", "bcachefs", "jfs", "reiserfs", "apfs",
            "ntfs", "ntfs3", "refs",
        ]),
        // FAT keeps file sizes in 32 bits
        max_file_size: is(&["vfat", "msdos", "fat", "fat32", "fat16"]).then_some(u32::MAX as u64),
        network: name.starts_with("fuse.sshfs")
            || is(&[
                "nfs",
                "nfs4",
                "cifs",
                "smb3",
                "smbfs",
                "9p",
                "afs",
                "ceph",
                "glusterfs",
                "davfs",
                "afpfs",
                "webdav",
            ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_file_systems() {
        assert!(classify("ext4").sparse && classify("EXT4").sparse);
        assert!(classify("apfs").sparse);
        assert!(!classify("exfat").sparse);
        assert_eq!(classify("vfat").max_file_size, Some(u32::MAX as u64));
        assert_eq!(classify("msdos").max_file_size, Some(u32::MAX as u64));
        assert_eq!(classify("exfat").max_file_size, None);
        assert!(classify("nfs4").network && classify("cifs").network && classify("fuse.sshfs").network);
        assert!(!classify("ext4").network);
    }

    #[test]
    fn something_unknown_is_assumed_to_need_all_the_room() {
        assert_eq!(
            classify("somefs"),
            Traits {
                sparse: false,
                max_file_size: None,
                network: false
            }
        );
    }
}
