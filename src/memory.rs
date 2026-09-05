//! A flat, byte addressable simulated RAM.
//!
//! Virtual addresses used by the kernel loader map directly onto offsets in this
//! space (an identity mapping), which keeps the model simple while still proving
//! the loader places bytes at the right address and zero fills the bss.

use crate::error::{BootError, BootResult};

/// A contiguous block of simulated physical memory.
#[derive(Clone, Debug)]
pub struct Memory {
    bytes: Vec<u8>,
}

impl Memory {
    /// Create `size` bytes of zeroed RAM.
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self { bytes: vec![0u8; size] }
    }

    /// Total size of the address space in bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    /// True when `[addr, addr + len)` fits inside the address space.
    #[must_use]
    pub fn in_range(&self, addr: u64, len: usize) -> bool {
        match usize::try_from(addr) {
            Ok(start) => start.checked_add(len).is_some_and(|end| end <= self.bytes.len()),
            Err(_) => false,
        }
    }

    /// Copy `data` to `addr`, failing if the range does not fit.
    pub fn write(&mut self, addr: u64, data: &[u8]) -> BootResult<()> {
        if !self.in_range(addr, data.len()) {
            return Err(BootError::Memory(format!(
                "write of {} bytes at {addr:#x} exceeds {} bytes of RAM",
                data.len(),
                self.bytes.len()
            )));
        }
        let start = addr as usize;
        self.bytes[start..start + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Zero `len` bytes at `addr`, failing if the range does not fit.
    pub fn zero(&mut self, addr: u64, len: usize) -> BootResult<()> {
        if !self.in_range(addr, len) {
            return Err(BootError::Memory(format!(
                "zero fill of {len} bytes at {addr:#x} exceeds {} bytes of RAM",
                self.bytes.len()
            )));
        }
        let start = addr as usize;
        for byte in &mut self.bytes[start..start + len] {
            *byte = 0;
        }
        Ok(())
    }

    /// Borrow `len` bytes at `addr`, failing if the range does not fit.
    pub fn read(&self, addr: u64, len: usize) -> BootResult<&[u8]> {
        if !self.in_range(addr, len) {
            return Err(BootError::Memory(format!(
                "read of {len} bytes at {addr:#x} exceeds {} bytes of RAM",
                self.bytes.len()
            )));
        }
        let start = addr as usize;
        Ok(&self.bytes[start..start + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrips() {
        let mut m = Memory::new(64);
        m.write(8, &[1, 2, 3, 4]).unwrap();
        assert_eq!(m.read(8, 4).unwrap(), &[1, 2, 3, 4]);
    }

    #[test]
    fn zero_fill_clears_bytes() {
        let mut m = Memory::new(64);
        m.write(0, &[9; 16]).unwrap();
        m.zero(4, 8).unwrap();
        assert_eq!(m.read(4, 8).unwrap(), &[0; 8]);
    }

    #[test]
    fn out_of_range_write_fails() {
        let mut m = Memory::new(16);
        assert!(m.write(12, &[1, 2, 3, 4, 5]).is_err());
    }

    #[test]
    fn out_of_range_read_fails() {
        let m = Memory::new(16);
        assert!(m.read(14, 4).is_err());
    }
}
