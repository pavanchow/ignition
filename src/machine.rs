//! The simulated machine: a disk, a flat memory space, and processor mode state.

use crate::cpu::Cpu;
use crate::disk::Disk;
use crate::memory::Memory;

/// Default size of simulated RAM, 16 MiB.
pub const DEFAULT_RAM_SIZE: usize = 16 * 1024 * 1024;

/// Where the firmware copies the MBR boot sector, the classic load address.
pub const MBR_LOAD_ADDR: u64 = 0x7C00;
/// Where stage 1 copies stage 2.
pub const STAGE2_LOAD_ADDR: u64 = 0x8000;
/// Where the loader writes the memory map for the kernel.
pub const MEMMAP_ADDR: u64 = 0x9000;
/// Where the stub kernel writes its result marker.
pub const KERNEL_OUTPUT_ADDR: u64 = 0xF000;
/// End of the low memory range reserved for firmware.
pub const LOW_RESERVED_END: u64 = 0x1000;
/// End of the low memory the loader keeps for itself (boot sector, stage 2, the
/// memory map, and the kernel output slot). A kernel image must load at or above
/// this address, mirroring how real bootloaders place protected mode kernels in
/// high memory and leave conventional low memory to the firmware and loader.
pub const LOADER_RESERVED_END: u64 = 0x0001_0000;

/// The whole simulated machine.
#[derive(Clone, Debug)]
pub struct Machine {
    /// The boot disk.
    pub disk: Disk,
    /// Physical RAM.
    pub memory: Memory,
    /// Processor mode state.
    pub cpu: Cpu,
}

impl Machine {
    /// Build a machine from a disk and a RAM size.
    #[must_use]
    pub fn new(disk: Disk, ram_size: usize) -> Self {
        Self {
            disk,
            memory: Memory::new(ram_size),
            cpu: Cpu::at_power_on(),
        }
    }
}
