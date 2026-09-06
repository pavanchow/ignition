//! A minimal filesystem superblock so stage 2 can locate the config and kernel
//! inside a partition, the way a real second stage reads a filesystem to find
//! files by path. It records where the boot config and the kernel images live,
//! measured in bytes from the start of the partition.
//!
//! The superblock names two kernel slots, A and B, so the loader can fall back
//! to a known good image when the preferred slot is corrupt. Slot A is required.
//! Slot B is optional and is marked absent by a zero length.
//!
//! On disk layout, 32 bytes, all integers little endian:
//! ```text
//! 0  magic     [u8;4] = "IGFS"
//! 4  version   u16
//! 6  _reserved u16
//! 8  config_offset   u32
//! 12 config_len      u32
//! 16 kernel_a_offset u32
//! 20 kernel_a_len    u32
//! 24 kernel_b_offset u32
//! 28 kernel_b_len    u32   (0 means slot B is absent)
//! ```

use crate::bytes::{read_u16, read_u32, write_u16, write_u32};
use crate::error::{BootError, BootResult};

/// Magic marker at the start of a partition formatted with this filesystem.
pub const IGFS_MAGIC: [u8; 4] = *b"IGFS";
/// The filesystem version this crate reads and writes.
pub const IGFS_VERSION: u16 = 2;
/// Encoded size of the superblock in bytes.
pub const SUPERBLOCK_SIZE: usize = 32;

/// A file region inside the partition: an offset and a length in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExtent {
    /// Byte offset of the file within the partition.
    pub offset: u32,
    /// Length of the file in bytes.
    pub len: u32,
}

impl FileExtent {
    /// One past the last byte this extent occupies, widened to avoid overflow.
    #[must_use]
    pub fn end(self) -> u64 {
        u64::from(self.offset) + u64::from(self.len)
    }

    /// True when the extent describes no bytes.
    #[must_use]
    pub fn is_absent(self) -> bool {
        self.len == 0
    }
}

/// A parsed superblock pointing at the config and the two kernel slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperBlock {
    /// The boot config extent.
    pub config: FileExtent,
    /// The primary kernel image extent (slot A), always present.
    pub kernel_a: FileExtent,
    /// The fallback kernel image extent (slot B), absent when its length is zero.
    pub kernel_b: FileExtent,
}

impl SuperBlock {
    /// Encode the superblock into its 32 byte on disk form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SUPERBLOCK_SIZE);
        out.extend_from_slice(&IGFS_MAGIC);
        write_u16(&mut out, IGFS_VERSION);
        write_u16(&mut out, 0); // reserved
        write_u32(&mut out, self.config.offset);
        write_u32(&mut out, self.config.len);
        write_u32(&mut out, self.kernel_a.offset);
        write_u32(&mut out, self.kernel_a.len);
        write_u32(&mut out, self.kernel_b.offset);
        write_u32(&mut out, self.kernel_b.len);
        out
    }

    /// True when a fallback slot B image is present.
    #[must_use]
    pub fn has_slot_b(&self) -> bool {
        !self.kernel_b.is_absent()
    }
}

/// Parse and validate a superblock from the start of a partition.
///
/// `partition_len` is the size of the containing partition in bytes. Every named
/// extent is checked to lie within the partition, to sit past the superblock, and
/// not to overlap the config extent, so a crafted superblock cannot point a read
/// outside its partition or make the config and a kernel alias one another.
pub fn parse_superblock(bytes: &[u8], partition_len: u64) -> BootResult<SuperBlock> {
    if bytes.len() < SUPERBLOCK_SIZE {
        return Err(BootError::Filesystem(format!(
            "superblock needs {SUPERBLOCK_SIZE} bytes, got {}",
            bytes.len()
        )));
    }
    if bytes[0..4] != IGFS_MAGIC {
        return Err(BootError::Filesystem("bad IGFS magic".into()));
    }
    let version = read_u16(bytes, 4).unwrap_or(0);
    if version != IGFS_VERSION {
        return Err(BootError::Filesystem(format!(
            "unsupported IGFS version {version}"
        )));
    }
    let config = FileExtent {
        offset: read_u32(bytes, 8).unwrap_or(0),
        len: read_u32(bytes, 12).unwrap_or(0),
    };
    let kernel_a = FileExtent {
        offset: read_u32(bytes, 16).unwrap_or(0),
        len: read_u32(bytes, 20).unwrap_or(0),
    };
    let kernel_b = FileExtent {
        offset: read_u32(bytes, 24).unwrap_or(0),
        len: read_u32(bytes, 28).unwrap_or(0),
    };

    if config.is_absent() {
        return Err(BootError::Filesystem("config length is zero".into()));
    }
    if kernel_a.is_absent() {
        return Err(BootError::Filesystem("slot A kernel length is zero".into()));
    }

    let sb = SuperBlock { config, kernel_a, kernel_b };
    validate_extents(&sb, partition_len)?;
    Ok(sb)
}

fn validate_extents(sb: &SuperBlock, partition_len: u64) -> BootResult<()> {
    let sb_size = SUPERBLOCK_SIZE as u64;
    let mut named: Vec<(&str, FileExtent)> = vec![("config", sb.config), ("kernel A", sb.kernel_a)];
    if sb.has_slot_b() {
        named.push(("kernel B", sb.kernel_b));
    }

    for (name, ext) in &named {
        if u64::from(ext.offset) < sb_size {
            return Err(BootError::Filesystem(format!(
                "{name} extent starts at {} inside the {sb_size} byte superblock",
                ext.offset
            )));
        }
        if ext.end() > partition_len {
            return Err(BootError::Filesystem(format!(
                "{name} extent ends at {} past the {partition_len} byte partition",
                ext.end()
            )));
        }
    }

    // The config must not alias either kernel image. Kernel A and B may share
    // nothing either, so a corrupt slot cannot silently be the good one.
    for i in 0..named.len() {
        for j in (i + 1)..named.len() {
            let (na, a) = named[i];
            let (nb, b) = named[j];
            let (sa, ea) = (u64::from(a.offset), a.end());
            let (sb2, eb) = (u64::from(b.offset), b.end());
            if sa < eb && sb2 < ea {
                return Err(BootError::Filesystem(format!(
                    "{na} and {nb} extents overlap in the partition"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(config: (u32, u32), a: (u32, u32), b: (u32, u32)) -> SuperBlock {
        SuperBlock {
            config: FileExtent { offset: config.0, len: config.1 },
            kernel_a: FileExtent { offset: a.0, len: a.1 },
            kernel_b: FileExtent { offset: b.0, len: b.1 },
        }
    }

    #[test]
    fn roundtrips_superblock() {
        let s = sb((32, 40), (72, 200), (272, 200));
        let parsed = parse_superblock(&s.encode(), 4096).unwrap();
        assert_eq!(parsed, s);
        assert!(parsed.has_slot_b());
    }

    #[test]
    fn single_slot_has_no_b() {
        let s = sb((32, 8), (40, 64), (0, 0));
        let parsed = parse_superblock(&s.encode(), 4096).unwrap();
        assert!(!parsed.has_slot_b());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut b = sb((32, 1), (40, 1), (0, 0)).encode();
        b[0] = 0;
        assert!(matches!(parse_superblock(&b, 4096), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_zero_slot_a_len() {
        let b = sb((32, 1), (40, 0), (0, 0)).encode();
        assert!(matches!(parse_superblock(&b, 4096), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_short_input() {
        assert!(matches!(parse_superblock(&[0u8; 4], 4096), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_extent_past_partition() {
        let b = sb((32, 8), (40, 5000), (0, 0)).encode();
        assert!(matches!(parse_superblock(&b, 4096), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_extent_inside_superblock() {
        let b = sb((4, 8), (40, 8), (0, 0)).encode();
        assert!(matches!(parse_superblock(&b, 4096), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_overlapping_extents() {
        let b = sb((32, 40), (60, 40), (0, 0)).encode();
        assert!(matches!(parse_superblock(&b, 4096), Err(BootError::Filesystem(_))));
    }
}
