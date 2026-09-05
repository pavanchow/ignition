//! The memory map handed from the loader to the kernel.
//!
//! A real bootloader gives the kernel a table describing which physical ranges
//! are usable RAM and which are reserved for firmware or already occupied by the
//! loaded image. We build the same idea: a deterministic, sorted list of typed
//! regions, encodable into bytes so the stub kernel can read it back.

use crate::bytes::{write_u32, write_u64};

/// What a memory region is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Low memory reserved for firmware structures.
    Reserved,
    /// Occupied by loader stages.
    Loader,
    /// Occupied by a loaded kernel segment.
    Kernel,
    /// Holds this memory map itself.
    MemoryMap,
    /// Usable free RAM.
    Free,
}

impl RegionKind {
    /// The stable numeric code written into the encoded map.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            RegionKind::Reserved => 0,
            RegionKind::Loader => 1,
            RegionKind::Kernel => 2,
            RegionKind::MemoryMap => 3,
            RegionKind::Free => 4,
        }
    }

    /// A human readable label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RegionKind::Reserved => "reserved",
            RegionKind::Loader => "loader",
            RegionKind::Kernel => "kernel",
            RegionKind::MemoryMap => "memmap",
            RegionKind::Free => "free",
        }
    }
}

/// A single typed physical memory range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    /// First byte of the region.
    pub base: u64,
    /// Length in bytes.
    pub size: u64,
    /// What the region is used for.
    pub kind: RegionKind,
}

/// A sorted, deterministic collection of memory regions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryMap {
    /// Regions in ascending base order.
    pub regions: Vec<MemoryRegion>,
}

impl MemoryMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a region and keep the list sorted by base address.
    pub fn add(&mut self, base: u64, size: u64, kind: RegionKind) {
        self.regions.push(MemoryRegion { base, size, kind });
        self.regions.sort_by_key(|r| r.base);
    }

    /// Number of regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// True when there are no regions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Encode as `u32` count followed by `[u64 base, u64 size, u32 kind]` records.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_u32(&mut out, self.regions.len() as u32);
        for r in &self.regions {
            write_u64(&mut out, r.base);
            write_u64(&mut out, r.size);
            write_u32(&mut out, r.kind.code());
        }
        out
    }
}
