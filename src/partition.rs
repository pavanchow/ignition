//! An MBR style partition table parser with a real validity check.
//!
//! The layout follows the classic master boot record: a 512 byte sector whose
//! last two bytes are the `0x55 0xAA` signature and whose four 16 byte partition
//! entries begin at offset 446.

use crate::bytes::read_u32;
use crate::disk::SECTOR_SIZE;
use crate::error::{BootError, BootResult};

/// Byte offset of the first partition entry inside the MBR sector.
pub const PARTITION_TABLE_OFFSET: usize = 446;
/// Size of a single partition entry in bytes.
pub const PARTITION_ENTRY_SIZE: usize = 16;
/// Number of primary partition entries in an MBR.
pub const PARTITION_ENTRY_COUNT: usize = 4;
/// The two byte boot signature that terminates a valid MBR.
pub const MBR_SIGNATURE: [u8; 2] = [0x55, 0xAA];
/// The `bootable` marker for the active partition.
pub const BOOTABLE_FLAG: u8 = 0x80;

/// One primary partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionEntry {
    /// True when the active (bootable) flag is set.
    pub bootable: bool,
    /// The partition type byte (0 means an unused slot).
    pub part_type: u8,
    /// First sector of the partition.
    pub start_lba: u32,
    /// Length of the partition in sectors.
    pub sector_count: u32,
}

impl PartitionEntry {
    /// True when this slot describes no partition.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.part_type == 0 && self.start_lba == 0 && self.sector_count == 0
    }

    /// One past the last sector of this partition.
    #[must_use]
    pub fn end_lba(&self) -> u64 {
        u64::from(self.start_lba) + u64::from(self.sector_count)
    }
}

/// A parsed and validated partition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTable {
    /// The four primary entries in on disk order.
    pub entries: [PartitionEntry; PARTITION_ENTRY_COUNT],
}

impl PartitionTable {
    /// The first entry marked bootable, if any.
    #[must_use]
    pub fn boot_partition(&self) -> Option<PartitionEntry> {
        self.entries.iter().copied().find(|e| e.bootable && !e.is_empty())
    }
}

/// Parse and validate the MBR held in `sector0`, given the disk size in sectors.
pub fn parse_mbr(sector0: &[u8], disk_sectors: u64) -> BootResult<PartitionTable> {
    if sector0.len() != SECTOR_SIZE {
        return Err(BootError::PartitionTable(format!(
            "MBR sector must be {SECTOR_SIZE} bytes, got {}",
            sector0.len()
        )));
    }
    if sector0[510..512] != MBR_SIGNATURE {
        return Err(BootError::BadBootSignature { sector: 0 });
    }

    let mut entries = [PartitionEntry {
        bootable: false,
        part_type: 0,
        start_lba: 0,
        sector_count: 0,
    }; PARTITION_ENTRY_COUNT];

    for (i, entry) in entries.iter_mut().enumerate() {
        let base = PARTITION_TABLE_OFFSET + i * PARTITION_ENTRY_SIZE;
        let flag = sector0[base];
        if flag != 0 && flag != BOOTABLE_FLAG {
            return Err(BootError::PartitionTable(format!(
                "entry {i} has invalid status byte {flag:#04x}"
            )));
        }
        let part_type = sector0[base + 4];
        // Offsets 1..4 and 5..8 are legacy CHS fields we intentionally ignore.
        let start_lba = read_u32(sector0, base + 8)
            .ok_or_else(|| BootError::PartitionTable(format!("entry {i} start LBA truncated")))?;
        let sector_count = read_u32(sector0, base + 12)
            .ok_or_else(|| BootError::PartitionTable(format!("entry {i} sector count truncated")))?;

        *entry = PartitionEntry {
            bootable: flag == BOOTABLE_FLAG,
            part_type,
            start_lba,
            sector_count,
        };
    }

    validate(&entries, disk_sectors)?;
    Ok(PartitionTable { entries })
}

fn validate(entries: &[PartitionEntry; PARTITION_ENTRY_COUNT], disk_sectors: u64) -> BootResult<()> {
    // A non empty entry must be self consistent and fit on the disk.
    for (i, e) in entries.iter().enumerate() {
        if e.is_empty() {
            continue;
        }
        if e.part_type != 0 && (e.start_lba == 0 || e.sector_count == 0) {
            return Err(BootError::PartitionTable(format!(
                "entry {i} has a type but a zero start or size"
            )));
        }
        if u64::from(e.start_lba) == 0 {
            return Err(BootError::PartitionTable(format!(
                "entry {i} starts inside the MBR sector"
            )));
        }
        if e.end_lba() > disk_sectors {
            return Err(BootError::PartitionTable(format!(
                "entry {i} runs to sector {} past the disk end {disk_sectors}",
                e.end_lba()
            )));
        }
    }

    // No two live partitions may overlap.
    for a in 0..PARTITION_ENTRY_COUNT {
        if entries[a].is_empty() {
            continue;
        }
        for b in (a + 1)..PARTITION_ENTRY_COUNT {
            if entries[b].is_empty() {
                continue;
            }
            let (sa, ea) = (u64::from(entries[a].start_lba), entries[a].end_lba());
            let (sb, eb) = (u64::from(entries[b].start_lba), entries[b].end_lba());
            if sa < eb && sb < ea {
                return Err(BootError::PartitionTable(format!(
                    "entries {a} and {b} overlap on disk"
                )));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_sector() -> Vec<u8> {
        let mut s = vec![0u8; SECTOR_SIZE];
        let base = PARTITION_TABLE_OFFSET;
        s[base] = BOOTABLE_FLAG;
        s[base + 4] = 0x83;
        s[base + 8..base + 12].copy_from_slice(&10u32.to_le_bytes());
        s[base + 12..base + 16].copy_from_slice(&20u32.to_le_bytes());
        s[510..512].copy_from_slice(&MBR_SIGNATURE);
        s
    }

    #[test]
    fn parses_valid_table() {
        let t = parse_mbr(&valid_sector(), 100).unwrap();
        let b = t.boot_partition().unwrap();
        assert!(b.bootable);
        assert_eq!(b.start_lba, 10);
        assert_eq!(b.sector_count, 20);
        assert_eq!(b.end_lba(), 30);
    }

    #[test]
    fn rejects_missing_signature() {
        let mut s = valid_sector();
        s[510] = 0;
        assert!(matches!(parse_mbr(&s, 100), Err(BootError::BadBootSignature { .. })));
    }

    #[test]
    fn rejects_partition_past_disk_end() {
        let s = valid_sector();
        assert!(matches!(parse_mbr(&s, 20), Err(BootError::PartitionTable(_))));
    }

    #[test]
    fn rejects_overlapping_partitions() {
        let mut s = valid_sector();
        let base = PARTITION_TABLE_OFFSET + PARTITION_ENTRY_SIZE;
        s[base + 4] = 0x83;
        s[base + 8..base + 12].copy_from_slice(&15u32.to_le_bytes());
        s[base + 12..base + 16].copy_from_slice(&20u32.to_le_bytes());
        assert!(matches!(parse_mbr(&s, 100), Err(BootError::PartitionTable(_))));
    }

    #[test]
    fn rejects_invalid_status_byte() {
        let mut s = valid_sector();
        s[PARTITION_TABLE_OFFSET] = 0x01;
        assert!(matches!(parse_mbr(&s, 100), Err(BootError::PartitionTable(_))));
    }
}
