//! Error types for every stage of the simulated boot chain.

use core::fmt;

/// A precise reason an ELF-like kernel image was rejected by the parser or loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElfError {
    /// The image is smaller than a valid header.
    TooSmall { have: usize, need: usize },
    /// The 4 byte magic did not match the expected value.
    BadMagic { found: [u8; 4] },
    /// The format version is not one this loader understands.
    UnsupportedVersion(u16),
    /// The image declares no loadable segments.
    NoSegments,
    /// The program header table lies outside the image bytes.
    HeaderTableOutOfRange { phoff: u32, phnum: u16, have: usize },
    /// A segment claims file bytes that lie outside the image.
    SegmentFileRange { index: usize },
    /// A segment claims a memory range that does not fit in RAM.
    SegmentMemoryRange { index: usize, vaddr: u64, memsz: u32, ram: usize },
    /// A segment declares a memory size smaller than its file size.
    MemLessThanFile { index: usize, filesz: u32, memsz: u32 },
    /// Two segments would occupy overlapping virtual address ranges.
    SegmentOverlap { a: usize, b: usize },
    /// The recorded entry point is not inside any executable segment.
    EntryNotExecutable { entry: u64 },
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::TooSmall { have, need } => {
                write!(f, "image too small: have {have} bytes, need at least {need}")
            }
            ElfError::BadMagic { found } => {
                write!(f, "bad kernel magic: found {found:02x?}")
            }
            ElfError::UnsupportedVersion(v) => write!(f, "unsupported image version {v}"),
            ElfError::NoSegments => write!(f, "image declares no loadable segments"),
            ElfError::HeaderTableOutOfRange { phoff, phnum, have } => write!(
                f,
                "program header table out of range: phoff {phoff} phnum {phnum} image {have} bytes"
            ),
            ElfError::SegmentFileRange { index } => {
                write!(f, "segment {index} file range exceeds image bytes")
            }
            ElfError::SegmentMemoryRange { index, vaddr, memsz, ram } => write!(
                f,
                "segment {index} memory range [{vaddr:#x}, +{memsz}) exceeds {ram} bytes of RAM"
            ),
            ElfError::MemLessThanFile { index, filesz, memsz } => write!(
                f,
                "segment {index} mem size {memsz} smaller than file size {filesz}"
            ),
            ElfError::SegmentOverlap { a, b } => {
                write!(f, "segments {a} and {b} overlap in virtual memory")
            }
            ElfError::EntryNotExecutable { entry } => {
                write!(f, "entry point {entry:#x} is not inside an executable segment")
            }
        }
    }
}

/// Any failure raised while walking the boot chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootError {
    /// Power on self test failed.
    Post(String),
    /// A sector did not carry the expected boot signature.
    BadBootSignature { sector: u64 },
    /// A stage blob did not carry its expected magic marker.
    BadStageMagic { stage: u8 },
    /// The partition table was malformed.
    PartitionTable(String),
    /// No partition was marked bootable.
    NoBootablePartition,
    /// The mini filesystem superblock was malformed.
    Filesystem(String),
    /// The boot configuration text was malformed.
    Config(String),
    /// The kernel image failed parsing or loading.
    Elf(ElfError),
    /// A memory access fell outside the simulated RAM.
    Memory(String),
    /// A disk access fell outside the image.
    Disk(String),
    /// The hand off preconditions were not met.
    Handoff(String),
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootError::Post(m) => write!(f, "POST failure: {m}"),
            BootError::BadBootSignature { sector } => {
                write!(f, "sector {sector} has no 0x55AA boot signature")
            }
            BootError::BadStageMagic { stage } => {
                write!(f, "stage {stage} blob is missing its magic marker")
            }
            BootError::PartitionTable(m) => write!(f, "partition table error: {m}"),
            BootError::NoBootablePartition => write!(f, "no bootable partition found"),
            BootError::Filesystem(m) => write!(f, "filesystem error: {m}"),
            BootError::Config(m) => write!(f, "boot config error: {m}"),
            BootError::Elf(e) => write!(f, "kernel image error: {e}"),
            BootError::Memory(m) => write!(f, "memory error: {m}"),
            BootError::Disk(m) => write!(f, "disk error: {m}"),
            BootError::Handoff(m) => write!(f, "hand off error: {m}"),
        }
    }
}

impl std::error::Error for BootError {}
impl std::error::Error for ElfError {}

impl From<ElfError> for BootError {
    fn from(e: ElfError) -> Self {
        BootError::Elf(e)
    }
}

/// Convenience alias used across the crate.
pub type BootResult<T> = Result<T, BootError>;
