//! Helpers that assemble a complete, well formed boot disk image, the inverse of
//! everything the boot chain reads. Used by the CLI demo and by the tests.

use crate::bytes::write_u32;
use crate::disk::SECTOR_SIZE;
use crate::elf::{self, SegmentSpec, FLAG_R, FLAG_W, FLAG_X};
use crate::igfs::{FileExtent, SuperBlock, SUPERBLOCK_SIZE};
use crate::partition::{BOOTABLE_FLAG, MBR_SIGNATURE, PARTITION_TABLE_OFFSET};

/// Magic marking the stage 1 boot sector.
pub const STAGE1_MAGIC: [u8; 4] = *b"IGN1";
/// Magic marking the stage 2 blob.
pub const STAGE2_MAGIC: [u8; 4] = *b"IGN2";
/// LBA of the stage 2 blob.
pub const STAGE2_LBA: u32 = 1;
/// Length of the stage 2 blob in sectors.
pub const STAGE2_SECTORS: u32 = 1;
/// First sector of the data partition.
pub const PARTITION_START_LBA: u32 = 2;
/// Partition type byte used for an Ignition boot partition.
pub const PARTITION_TYPE: u8 = 0x7F;

fn round_up(value: usize, multiple: usize) -> usize {
    value.div_ceil(multiple) * multiple
}

/// Build the 512 byte stage 2 blob.
#[must_use]
pub fn build_stage2() -> Vec<u8> {
    let mut s = Vec::with_capacity(SECTOR_SIZE);
    s.extend_from_slice(&STAGE2_MAGIC);
    s.resize(SECTOR_SIZE, 0);
    s
}

/// Build the partition payload: superblock, config text, then one or two kernel
/// images laid out back to back. Passing a slot B image enables A/B fallback.
#[must_use]
pub fn build_partition(config_text: &str, kernel_a: &[u8], kernel_b: Option<&[u8]>) -> Vec<u8> {
    let config_bytes = config_text.as_bytes();
    let config_offset = SUPERBLOCK_SIZE;
    let kernel_a_offset = config_offset + config_bytes.len();
    let kernel_b_offset = kernel_a_offset + kernel_a.len();

    let sb = SuperBlock {
        config: FileExtent {
            offset: config_offset as u32,
            len: config_bytes.len() as u32,
        },
        kernel_a: FileExtent {
            offset: kernel_a_offset as u32,
            len: kernel_a.len() as u32,
        },
        kernel_b: match kernel_b {
            Some(b) => FileExtent { offset: kernel_b_offset as u32, len: b.len() as u32 },
            None => FileExtent { offset: 0, len: 0 },
        },
    };

    let mut part = Vec::new();
    part.extend_from_slice(&sb.encode());
    part.extend_from_slice(config_bytes);
    part.extend_from_slice(kernel_a);
    if let Some(b) = kernel_b {
        part.extend_from_slice(b);
    }
    let padded = round_up(part.len().max(1), SECTOR_SIZE);
    part.resize(padded, 0);
    part
}

/// Build a full MBR sector given the data partition size in sectors.
#[must_use]
pub fn build_mbr(partition_sectors: u32) -> Vec<u8> {
    let mut mbr = vec![0u8; SECTOR_SIZE];
    // Stage 1 boot parameter block.
    mbr[0..4].copy_from_slice(&STAGE1_MAGIC);
    let mut bpb = Vec::new();
    write_u32(&mut bpb, STAGE2_LBA);
    write_u32(&mut bpb, STAGE2_SECTORS);
    mbr[4..12].copy_from_slice(&bpb);

    // One bootable primary partition at offset 446.
    let base = PARTITION_TABLE_OFFSET;
    mbr[base] = BOOTABLE_FLAG;
    mbr[base + 4] = PARTITION_TYPE;
    mbr[base + 8..base + 12].copy_from_slice(&PARTITION_START_LBA.to_le_bytes());
    mbr[base + 12..base + 16].copy_from_slice(&partition_sectors.to_le_bytes());

    // Boot signature.
    mbr[510..512].copy_from_slice(&MBR_SIGNATURE);
    mbr
}

/// Assemble a complete disk image from a boot config and a single kernel image.
#[must_use]
pub fn build_disk(config_text: &str, kernel_image: &[u8]) -> Vec<u8> {
    build_disk_ab(config_text, kernel_image, None)
}

/// Assemble a complete disk image with an A slot and an optional B fallback slot.
#[must_use]
pub fn build_disk_ab(config_text: &str, kernel_a: &[u8], kernel_b: Option<&[u8]>) -> Vec<u8> {
    let stage2 = build_stage2();
    let partition = build_partition(config_text, kernel_a, kernel_b);
    let partition_sectors = (partition.len() / SECTOR_SIZE) as u32;
    let mbr = build_mbr(partition_sectors);

    let total_sectors = PARTITION_START_LBA as usize + partition.len() / SECTOR_SIZE;
    let mut disk = vec![0u8; total_sectors * SECTOR_SIZE];

    disk[0..SECTOR_SIZE].copy_from_slice(&mbr);
    let s2 = STAGE2_LBA as usize * SECTOR_SIZE;
    disk[s2..s2 + stage2.len()].copy_from_slice(&stage2);
    let p = PARTITION_START_LBA as usize * SECTOR_SIZE;
    disk[p..p + partition.len()].copy_from_slice(&partition);
    disk
}

/// The standard config text shipped in the demo image.
#[must_use]
pub fn demo_config() -> String {
    "# Ignition demo boot config\n\
     kernel = /boot/vmignition\n\
     cmdline = quiet loglevel=3\n\
     timeout = 5\n"
        .to_string()
}

/// A small but complete kernel image with text, rodata, and a pure bss segment.
#[must_use]
pub fn demo_kernel_image() -> Vec<u8> {
    let text: Vec<u8> = b"\xB8\x2A\x00\x00\x00\xC3IGNITION-TEXT".to_vec();
    let rodata: Vec<u8> = b"hello from the ignition demo kernel".to_vec();
    let segments = vec![
        SegmentSpec {
            vaddr: 0x0010_0000,
            memsz: text.len() as u32,
            flags: FLAG_R | FLAG_X,
            data: text,
        },
        SegmentSpec {
            vaddr: 0x0010_1000,
            memsz: rodata.len() as u32,
            flags: FLAG_R,
            data: rodata,
        },
        SegmentSpec {
            // A pure bss segment: no file bytes, 4 KiB of zeroed memory.
            vaddr: 0x0010_2000,
            memsz: 0x1000,
            flags: FLAG_R | FLAG_W,
            data: Vec::new(),
        },
    ];
    elf::build_image(0x0010_0000, &segments)
}

/// Build the standard, bootable demo disk image.
#[must_use]
pub fn build_demo_disk() -> Vec<u8> {
    build_disk(&demo_config(), &demo_kernel_image())
}

/// A copy of the demo kernel image with its magic corrupted, so it always fails
/// to load. Used to demonstrate A/B fallback.
#[must_use]
pub fn corrupt_kernel_image() -> Vec<u8> {
    let mut img = demo_kernel_image();
    img[0] = b'X';
    img
}

/// Build a demo disk whose slot A is corrupt and whose slot B is the good demo
/// kernel, so a boot of this image falls back from A to B.
#[must_use]
pub fn build_ab_demo_disk() -> Vec<u8> {
    build_disk_ab(&demo_config(), &corrupt_kernel_image(), Some(&demo_kernel_image()))
}
