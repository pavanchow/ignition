//! The processor mode state, modelling the real mode to protected mode switch.
//!
//! Real x86 machines start in 16 bit real mode with the A20 line masked, so only
//! the low megabyte is reachable. A bootloader enables the A20 gate and sets the
//! protection enable bit to move to 32 bit protected mode before jumping to a
//! modern kernel. We model that as explicit, checkable state.

use crate::error::{BootError, BootResult};

/// The execution mode of the simulated processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuMode {
    /// 16 bit mode entered at power on.
    RealMode,
    /// 32 bit flat mode entered after the switch.
    ProtectedMode,
}

/// The observable processor state relevant to booting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    /// Current execution mode.
    pub mode: CpuMode,
    /// Whether the A20 address line is enabled (memory above 1 MiB reachable).
    pub a20: bool,
    /// The instruction pointer, meaningful once execution starts.
    pub ip: u64,
}

impl Cpu {
    /// A freshly powered processor: real mode, A20 masked.
    #[must_use]
    pub fn at_power_on() -> Self {
        Self { mode: CpuMode::RealMode, a20: false, ip: 0 }
    }

    /// Enable the A20 gate so the full address space becomes reachable.
    pub fn enable_a20(&mut self) {
        self.a20 = true;
    }

    /// Switch to protected mode. Fails if A20 is still masked or we already switched.
    pub fn enter_protected_mode(&mut self) -> BootResult<()> {
        if self.mode == CpuMode::ProtectedMode {
            return Err(BootError::Handoff("already in protected mode".into()));
        }
        if !self.a20 {
            return Err(BootError::Handoff(
                "cannot enter protected mode with A20 masked".into(),
            ));
        }
        self.mode = CpuMode::ProtectedMode;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_in_real_mode_with_a20_masked() {
        let c = Cpu::at_power_on();
        assert_eq!(c.mode, CpuMode::RealMode);
        assert!(!c.a20);
    }

    #[test]
    fn cannot_switch_without_a20() {
        let mut c = Cpu::at_power_on();
        assert!(c.enter_protected_mode().is_err());
        assert_eq!(c.mode, CpuMode::RealMode);
    }

    #[test]
    fn switches_after_enabling_a20() {
        let mut c = Cpu::at_power_on();
        c.enable_a20();
        c.enter_protected_mode().unwrap();
        assert_eq!(c.mode, CpuMode::ProtectedMode);
    }

    #[test]
    fn double_switch_is_rejected() {
        let mut c = Cpu::at_power_on();
        c.enable_a20();
        c.enter_protected_mode().unwrap();
        assert!(c.enter_protected_mode().is_err());
    }
}
