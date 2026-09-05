//! An ELF-like kernel image format and its loader, the testable core of Ignition.
//!
//! The format is deliberately a small, faithful cousin of ELF rather than ELF
//! itself, so the parser and loader stay readable while still exercising every
//! idea that matters: a magic, an entry point, a program header table, and
//! segments that carry a file size and a possibly larger memory size (the extra
//! being bss that the loader must zero fill).
//!
//! On disk layout, all integers little endian.
//!
//! Header, 24 bytes:
//! ```text
//! 0  magic   [u8;4] = "KIMG"
//! 4  version u16
//! 6  flags   u16    (reserved)
//! 8  entry   u64    virtual address of the entry point
//! 16 phoff   u32    file offset of the program header table
//! 20 phnum   u16    number of program headers
//! 22 _pad    u16
//! ```
//!
//! Program header, 28 bytes each:
//! ```text
//! 0  p_offset u32   file offset of the segment bytes
//! 4  p_vaddr  u64   destination virtual address
//! 12 p_filesz u32   bytes present in the file
//! 16 p_memsz  u32   bytes occupied in memory (>= p_filesz, extra is bss)
//! 20 p_flags  u32   permission bits
//! 24 _pad     u32
//! ```

use crate::bytes::{read_u16, read_u32, read_u64, write_u16, write_u32, write_u64};
use crate::error::ElfError;
use crate::memory::Memory;

/// Magic bytes at the start of every kernel image.
pub const KIMG_MAGIC: [u8; 4] = *b"KIMG";
/// The format version this crate reads and writes.
pub const KIMG_VERSION: u16 = 1;
/// Encoded size of the file header.
pub const HEADER_SIZE: usize = 24;
/// Encoded size of one program header.
pub const PROGRAM_HEADER_SIZE: usize = 28;

/// Segment is executable.
pub const FLAG_X: u32 = 0x1;
/// Segment is writable.
pub const FLAG_W: u32 = 0x2;
/// Segment is readable.
pub const FLAG_R: u32 = 0x4;

/// The parsed file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelHeader {
    /// Format version.
    pub version: u16,
    /// Virtual address of the entry point.
    pub entry: u64,
    /// File offset of the program header table.
    pub phoff: u32,
    /// Number of program headers.
    pub phnum: u16,
}

/// One program header as read from the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramHeader {
    /// File offset of the segment bytes.
    pub offset: u32,
    /// Destination virtual address.
    pub vaddr: u64,
    /// Bytes present in the file.
    pub filesz: u32,
    /// Bytes occupied in memory.
    pub memsz: u32,
    /// Permission flags.
    pub flags: u32,
}

impl ProgramHeader {
    /// One past the last virtual address this segment occupies.
    #[must_use]
    pub fn vend(&self) -> u64 {
        self.vaddr + u64::from(self.memsz)
    }

    /// True when the executable bit is set.
    #[must_use]
    pub fn is_executable(&self) -> bool {
        self.flags & FLAG_X != 0
    }

    /// A short `rwx` style permission string.
    #[must_use]
    pub fn perm_string(&self) -> String {
        let r = if self.flags & FLAG_R != 0 { 'r' } else { '-' };
        let w = if self.flags & FLAG_W != 0 { 'w' } else { '-' };
        let x = if self.flags & FLAG_X != 0 { 'x' } else { '-' };
        format!("{r}{w}{x}")
    }
}

/// A segment after it has been placed in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedSegment {
    /// Virtual address the segment was written to.
    pub vaddr: u64,
    /// Bytes copied from the file.
    pub filesz: u32,
    /// Total bytes occupied, including zero filled bss.
    pub memsz: u32,
    /// Permission flags.
    pub flags: u32,
}

impl LoadedSegment {
    /// Number of zero filled bss bytes after the file bytes.
    #[must_use]
    pub fn bss_len(&self) -> u32 {
        self.memsz - self.filesz
    }
}

/// The result of loading a kernel image into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedKernel {
    /// The entry point virtual address.
    pub entry: u64,
    /// Segments in the order they were loaded.
    pub segments: Vec<LoadedSegment>,
}

/// Parse the file header, validating magic and version.
pub fn parse_header(image: &[u8]) -> Result<KernelHeader, ElfError> {
    if image.len() < HEADER_SIZE {
        return Err(ElfError::TooSmall { have: image.len(), need: HEADER_SIZE });
    }
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&image[0..4]);
    if magic != KIMG_MAGIC {
        return Err(ElfError::BadMagic { found: magic });
    }
    let version = read_u16(image, 4).ok_or(ElfError::TooSmall { have: image.len(), need: HEADER_SIZE })?;
    if version != KIMG_VERSION {
        return Err(ElfError::UnsupportedVersion(version));
    }
    let entry = read_u64(image, 8).ok_or(ElfError::TooSmall { have: image.len(), need: HEADER_SIZE })?;
    let phoff = read_u32(image, 16).ok_or(ElfError::TooSmall { have: image.len(), need: HEADER_SIZE })?;
    let phnum = read_u16(image, 20).ok_or(ElfError::TooSmall { have: image.len(), need: HEADER_SIZE })?;

    if phnum == 0 {
        return Err(ElfError::NoSegments);
    }
    Ok(KernelHeader { version, entry, phoff, phnum })
}

/// Parse the program header table described by `header`.
pub fn parse_program_headers(image: &[u8], header: &KernelHeader) -> Result<Vec<ProgramHeader>, ElfError> {
    let phoff = header.phoff as usize;
    let table_len = (header.phnum as usize)
        .checked_mul(PROGRAM_HEADER_SIZE)
        .ok_or(ElfError::HeaderTableOutOfRange {
            phoff: header.phoff,
            phnum: header.phnum,
            have: image.len(),
        })?;
    let end = phoff
        .checked_add(table_len)
        .ok_or(ElfError::HeaderTableOutOfRange {
            phoff: header.phoff,
            phnum: header.phnum,
            have: image.len(),
        })?;
    if end > image.len() {
        return Err(ElfError::HeaderTableOutOfRange {
            phoff: header.phoff,
            phnum: header.phnum,
            have: image.len(),
        });
    }

    let mut headers = Vec::with_capacity(header.phnum as usize);
    for i in 0..header.phnum as usize {
        let base = phoff + i * PROGRAM_HEADER_SIZE;
        let offset = read_u32(image, base).ok_or(ElfError::SegmentFileRange { index: i })?;
        let vaddr = read_u64(image, base + 4).ok_or(ElfError::SegmentFileRange { index: i })?;
        let filesz = read_u32(image, base + 12).ok_or(ElfError::SegmentFileRange { index: i })?;
        let memsz = read_u32(image, base + 16).ok_or(ElfError::SegmentFileRange { index: i })?;
        let flags = read_u32(image, base + 20).ok_or(ElfError::SegmentFileRange { index: i })?;
        headers.push(ProgramHeader { offset, vaddr, filesz, memsz, flags });
    }
    Ok(headers)
}

/// Validate every program header against the image and the size of RAM.
pub fn validate_segments(
    image: &[u8],
    headers: &[ProgramHeader],
    ram: usize,
) -> Result<(), ElfError> {
    if headers.is_empty() {
        return Err(ElfError::NoSegments);
    }
    for (i, h) in headers.iter().enumerate() {
        if h.memsz < h.filesz {
            return Err(ElfError::MemLessThanFile { index: i, filesz: h.filesz, memsz: h.memsz });
        }
        // File bytes must lie inside the image.
        let file_end = (h.offset as usize)
            .checked_add(h.filesz as usize)
            .ok_or(ElfError::SegmentFileRange { index: i })?;
        if file_end > image.len() {
            return Err(ElfError::SegmentFileRange { index: i });
        }
        // Memory range must lie inside RAM.
        let mem_end = h
            .vaddr
            .checked_add(u64::from(h.memsz))
            .ok_or(ElfError::SegmentMemoryRange { index: i, vaddr: h.vaddr, memsz: h.memsz, ram })?;
        if mem_end > ram as u64 {
            return Err(ElfError::SegmentMemoryRange { index: i, vaddr: h.vaddr, memsz: h.memsz, ram });
        }
    }

    // No two loadable segments may overlap in virtual memory.
    for a in 0..headers.len() {
        for b in (a + 1)..headers.len() {
            let (sa, ea) = (headers[a].vaddr, headers[a].vend());
            let (sb, eb) = (headers[b].vaddr, headers[b].vend());
            if sa < eb && sb < ea {
                return Err(ElfError::SegmentOverlap { a, b });
            }
        }
    }
    Ok(())
}

/// Parse, validate, and load a kernel image into `memory`.
///
/// Each segment's file bytes are copied to its virtual address, the bss region
/// beyond the file size is zero filled, and the entry point is confirmed to fall
/// inside an executable segment. Nothing outside a declared segment is touched.
pub fn load(image: &[u8], memory: &mut Memory) -> Result<LoadedKernel, ElfError> {
    let header = parse_header(image)?;
    let headers = parse_program_headers(image, &header)?;
    validate_segments(image, &headers, memory.size())?;

    // Confirm the entry point is inside an executable segment before mutating RAM.
    let entry_ok = headers
        .iter()
        .any(|h| h.is_executable() && h.vaddr <= header.entry && header.entry < h.vend());
    if !entry_ok {
        return Err(ElfError::EntryNotExecutable { entry: header.entry });
    }

    let mut segments = Vec::with_capacity(headers.len());
    for h in &headers {
        let start = h.offset as usize;
        let file_bytes = &image[start..start + h.filesz as usize];
        // Copy the file bytes to the virtual address.
        memory
            .write(h.vaddr, file_bytes)
            .map_err(|_| ElfError::SegmentMemoryRange {
                index: 0,
                vaddr: h.vaddr,
                memsz: h.memsz,
                ram: memory.size(),
            })?;
        // Zero fill the bss.
        let bss = h.memsz - h.filesz;
        if bss > 0 {
            memory
                .zero(h.vaddr + u64::from(h.filesz), bss as usize)
                .map_err(|_| ElfError::SegmentMemoryRange {
                    index: 0,
                    vaddr: h.vaddr,
                    memsz: h.memsz,
                    ram: memory.size(),
                })?;
        }
        segments.push(LoadedSegment {
            vaddr: h.vaddr,
            filesz: h.filesz,
            memsz: h.memsz,
            flags: h.flags,
        });
    }

    Ok(LoadedKernel { entry: header.entry, segments })
}

/// A segment description used to assemble an image with [`build_image`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentSpec {
    /// Destination virtual address.
    pub vaddr: u64,
    /// The actual bytes to place in the file (and thus in memory).
    pub data: Vec<u8>,
    /// Total memory size, must be at least `data.len()`.
    pub memsz: u32,
    /// Permission flags.
    pub flags: u32,
}

/// Assemble a well formed kernel image from an entry point and a set of segments.
///
/// Segment file bytes are laid out after the header and program header table, in
/// order. This is the inverse of [`load`] and is used by the CLI and the tests.
#[must_use]
pub fn build_image(entry: u64, segments: &[SegmentSpec]) -> Vec<u8> {
    let phoff = HEADER_SIZE;
    let phnum = segments.len();
    let data_start = phoff + phnum * PROGRAM_HEADER_SIZE;

    // Header.
    let mut out = Vec::new();
    out.extend_from_slice(&KIMG_MAGIC);
    write_u16(&mut out, KIMG_VERSION);
    write_u16(&mut out, 0); // flags
    write_u64(&mut out, entry);
    write_u32(&mut out, phoff as u32);
    write_u16(&mut out, phnum as u16);
    write_u16(&mut out, 0); // pad

    // Program header table, with file offsets computed as we go.
    let mut cursor = data_start;
    for seg in segments {
        write_u32(&mut out, cursor as u32);
        write_u64(&mut out, seg.vaddr);
        write_u32(&mut out, seg.data.len() as u32);
        write_u32(&mut out, seg.memsz);
        write_u32(&mut out, seg.flags);
        write_u32(&mut out, 0); // pad
        cursor += seg.data.len();
    }

    // Segment payloads.
    for seg in segments {
        out.extend_from_slice(&seg.data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    fn one_seg_image() -> Vec<u8> {
        build_image(
            0x1000,
            &[SegmentSpec { vaddr: 0x1000, memsz: 32, flags: FLAG_R | FLAG_X, data: vec![7; 8] }],
        )
    }

    #[test]
    fn parses_header() {
        let h = parse_header(&one_seg_image()).unwrap();
        assert_eq!(h.entry, 0x1000);
        assert_eq!(h.phnum, 1);
        assert_eq!(h.version, KIMG_VERSION);
    }

    #[test]
    fn loads_and_zero_fills_bss() {
        let img = one_seg_image();
        let mut mem = Memory::new(1 << 16);
        let loaded = load(&img, &mut mem).unwrap();
        assert_eq!(loaded.entry, 0x1000);
        assert_eq!(mem.read(0x1000, 8).unwrap(), &[7; 8]);
        // memsz 32, filesz 8, so 24 bytes of bss must be zero.
        assert_eq!(mem.read(0x1008, 24).unwrap(), &[0; 24]);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut img = one_seg_image();
        img[1] = b'Z';
        let mut mem = Memory::new(1 << 16);
        assert!(matches!(load(&img, &mut mem), Err(ElfError::BadMagic { .. })));
    }

    #[test]
    fn perm_string_reads_flags() {
        let ph = ProgramHeader { offset: 0, vaddr: 0, filesz: 0, memsz: 1, flags: FLAG_R | FLAG_X };
        assert_eq!(ph.perm_string(), "r-x");
    }
}
