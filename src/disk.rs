//! A sector addressable block device backing the boot image.

use crate::error::{BootError, BootResult};

/// The fixed sector size the simulator uses, matching classic 512 byte sectors.
pub const SECTOR_SIZE: usize = 512;

/// A read only disk made of `SECTOR_SIZE` byte sectors.
#[derive(Clone, Debug)]
pub struct Disk {
    data: Vec<u8>,
}

impl Disk {
    /// Wrap a raw image. The image length must be a whole number of sectors.
    pub fn from_image(data: Vec<u8>) -> BootResult<Self> {
        if data.is_empty() || !data.len().is_multiple_of(SECTOR_SIZE) {
            return Err(BootError::Disk(format!(
                "image length {} is not a positive multiple of {SECTOR_SIZE}",
                data.len()
            )));
        }
        Ok(Self { data })
    }

    /// Number of whole sectors on the disk.
    #[must_use]
    pub fn sector_count(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE) as u64
    }

    /// Borrow a single sector identified by its logical block address.
    pub fn read_sector(&self, lba: u64) -> BootResult<&[u8]> {
        self.read_sectors(lba, 1)
    }

    /// Borrow `count` consecutive sectors starting at `lba`.
    pub fn read_sectors(&self, lba: u64, count: u64) -> BootResult<&[u8]> {
        if count == 0 {
            return Err(BootError::Disk("sector read count must not be zero".into()));
        }
        let start = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or_else(|| BootError::Disk("sector offset overflow".into()))?;
        let len = count
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or_else(|| BootError::Disk("sector length overflow".into()))?;
        self.read_bytes(start, len)
    }

    /// Borrow `len` raw bytes at a byte `offset` into the image.
    pub fn read_bytes(&self, offset: u64, len: u64) -> BootResult<&[u8]> {
        let start = usize::try_from(offset)
            .map_err(|_| BootError::Disk("offset does not fit in usize".into()))?;
        let count = usize::try_from(len)
            .map_err(|_| BootError::Disk("length does not fit in usize".into()))?;
        let end = start
            .checked_add(count)
            .ok_or_else(|| BootError::Disk("range overflow".into()))?;
        self.data
            .get(start..end)
            .ok_or_else(|| BootError::Disk(format!(
                "range [{start}, {end}) exceeds image of {} bytes",
                self.data.len()
            )))
    }
}
