//! The kernel side of the hand off.
//!
//! Because Ignition is a simulator in safe std Rust, it cannot execute the raw
//! machine code inside a loaded image. Instead a loaded image is represented at
//! run time by a [`Kernel`] implementation, and the hand off "jumps" to it by
//! calling [`Kernel::run`]. The built in [`StubKernel`] models exactly what a
//! tiny kernel does first: read the memory map the loader left for it, then write
//! a proof of life marker back into RAM.

use crate::bytes::read_u32;
use crate::error::{BootError, BootResult};
use crate::machine::{Machine, KERNEL_OUTPUT_ADDR};

/// The marker a healthy stub kernel writes once it is running.
pub const KERNEL_OK_MARKER: &[u8] = b"IGNITED\0";

/// State the loader passes to the kernel at the moment of hand off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handoff {
    /// Entry point the loader jumped to.
    pub entry: u64,
    /// Address of the memory map in RAM.
    pub memmap_addr: u64,
}

/// What the kernel produced, used to prove the hand off worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelOutcome {
    /// The marker string the kernel wrote.
    pub marker: String,
    /// How many memory map regions the kernel read.
    pub regions_seen: u32,
    /// Where the kernel wrote its marker.
    pub output_addr: u64,
}

/// A loaded kernel that can be run after hand off.
pub trait Kernel {
    /// A short name for logging.
    fn name(&self) -> &'static str;
    /// Run the kernel, given the machine and the hand off state.
    fn run(&self, machine: &mut Machine, handoff: &Handoff) -> BootResult<KernelOutcome>;
}

/// The default tiny kernel used by the demo and the tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubKernel;

impl Kernel for StubKernel {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn run(&self, machine: &mut Machine, handoff: &Handoff) -> BootResult<KernelOutcome> {
        // Read the region count the loader left in the memory map.
        let header = machine.memory.read(handoff.memmap_addr, 4)?;
        let regions_seen = read_u32(header, 0)
            .ok_or_else(|| BootError::Handoff("memory map header unreadable".into()))?;
        // Write the proof of life marker.
        machine.memory.write(KERNEL_OUTPUT_ADDR, KERNEL_OK_MARKER)?;
        Ok(KernelOutcome {
            marker: String::from_utf8_lossy(KERNEL_OK_MARKER)
                .trim_end_matches('\0')
                .to_string(),
            regions_seen,
            output_addr: KERNEL_OUTPUT_ADDR,
        })
    }
}

/// Read back the marker the kernel wrote, for verification.
pub fn read_marker(machine: &Machine, addr: u64) -> BootResult<Vec<u8>> {
    Ok(machine.memory.read(addr, KERNEL_OK_MARKER.len())?.to_vec())
}
