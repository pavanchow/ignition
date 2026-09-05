//! Ignition is a deterministic boot sequence simulator written in pure, safe
//! std Rust with zero external dependencies.
//!
//! A real bootloader runs on bare metal and cannot be unit tested with `cargo
//! test` or run in a browser. Ignition instead models the whole boot chain, from
//! firmware POST through the hand off to a kernel, while implementing the parts
//! that are genuinely testable as real code: an MBR style partition table, a
//! boot configuration format, and an ELF-like kernel image with a loader that
//! places segments at their virtual addresses and zero fills the bss. It is a
//! teaching accurate model, not a bootable artifact.
//!
//! The modules mirror the boot chain:
//! - [`disk`] and [`memory`] and [`cpu`] make up the simulated [`machine`].
//! - [`partition`], [`config`], [`igfs`], and [`elf`] are the real parsers.
//! - [`memmap`] and [`kernel`] describe the hand off.
//! - [`boot`] walks every stage in order and produces a deterministic report.
//! - [`image`] assembles disk images, the inverse of the parsers.

pub mod boot;
pub mod bytes;
pub mod config;
pub mod cpu;
pub mod disk;
pub mod elf;
pub mod error;
pub mod igfs;
pub mod image;
pub mod kernel;
pub mod machine;
pub mod memmap;
pub mod memory;
pub mod partition;

pub use boot::{boot, boot_default, BootReport};
pub use error::{BootError, BootResult, ElfError};
