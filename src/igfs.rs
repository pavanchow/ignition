//! A minimal filesystem superblock so stage 2 can locate the config and kernel
//! inside a partition, the way a real second stage reads a filesystem to find
//! files by path. It records where the boot config and the kernel image live,
//! measured in bytes from the start of the partition.

use crate::bytes::{read_u32, write_u16, write_u32};
use crate::error::{BootError, BootResult};

/// Magic marker at the start of a partition formatted with this filesystem.
pub const IGFS_MAGIC: [u8; 4] = *b"IGFS";
/// The filesystem version this crate reads and writes.
pub const IGFS_VERSION: u16 = 1;
/// Encoded size of the superblock in bytes.
pub const SUPERBLOCK_SIZE: usize = 24;

/// A parsed superblock pointing at the two files stage 2 needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperBlock {
    /// Byte offset of the boot config within the partition.
    pub config_offset: u32,
    /// Length of the boot config in bytes.
    pub config_len: u32,
    /// Byte offset of the kernel image within the partition.
    pub kernel_offset: u32,
    /// Length of the kernel image in bytes.
    pub kernel_len: u32,
}

impl SuperBlock {
    /// Encode the superblock into its 24 byte on disk form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SUPERBLOCK_SIZE);
        out.extend_from_slice(&IGFS_MAGIC);
        write_u16(&mut out, IGFS_VERSION);
        write_u16(&mut out, 0); // reserved
        write_u32(&mut out, self.config_offset);
        write_u32(&mut out, self.config_len);
        write_u32(&mut out, self.kernel_offset);
        write_u32(&mut out, self.kernel_len);
        out
    }
}

/// Parse and validate a superblock from the start of a partition.
pub fn parse_superblock(bytes: &[u8]) -> BootResult<SuperBlock> {
    if bytes.len() < SUPERBLOCK_SIZE {
        return Err(BootError::Filesystem(format!(
            "superblock needs {SUPERBLOCK_SIZE} bytes, got {}",
            bytes.len()
        )));
    }
    if bytes[0..4] != IGFS_MAGIC {
        return Err(BootError::Filesystem("bad IGFS magic".into()));
    }
    let version = crate::bytes::read_u16(bytes, 4).unwrap_or(0);
    if version != IGFS_VERSION {
        return Err(BootError::Filesystem(format!(
            "unsupported IGFS version {version}"
        )));
    }
    let config_offset = read_u32(bytes, 8).unwrap_or(0);
    let config_len = read_u32(bytes, 12).unwrap_or(0);
    let kernel_offset = read_u32(bytes, 16).unwrap_or(0);
    let kernel_len = read_u32(bytes, 20).unwrap_or(0);

    if config_len == 0 {
        return Err(BootError::Filesystem("config length is zero".into()));
    }
    if kernel_len == 0 {
        return Err(BootError::Filesystem("kernel length is zero".into()));
    }

    Ok(SuperBlock { config_offset, config_len, kernel_offset, kernel_len })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_superblock() {
        let sb = SuperBlock { config_offset: 24, config_len: 40, kernel_offset: 64, kernel_len: 200 };
        let parsed = parse_superblock(&sb.encode()).unwrap();
        assert_eq!(parsed, sb);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut b = SuperBlock { config_offset: 24, config_len: 1, kernel_offset: 25, kernel_len: 1 }.encode();
        b[0] = 0;
        assert!(matches!(parse_superblock(&b), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_zero_kernel_len() {
        let b = SuperBlock { config_offset: 24, config_len: 1, kernel_offset: 25, kernel_len: 0 }.encode();
        assert!(matches!(parse_superblock(&b), Err(BootError::Filesystem(_))));
    }

    #[test]
    fn rejects_short_input() {
        assert!(matches!(parse_superblock(&[0u8; 4]), Err(BootError::Filesystem(_))));
    }
}
