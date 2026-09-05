# Ignition

A deterministic boot sequence simulator written in pure, safe Rust with zero external dependencies, edition 2021.

Live playground: https://pavanchow.github.io/ignition/

## What it is, honestly

A real bootloader runs on bare metal. It cannot be exercised with `cargo test` and it cannot run inside a web page. Pretending otherwise would teach the wrong thing.

Ignition takes the honest path. It is a teaching-accurate model of the whole boot chain, from firmware power-on through the hand-off to a kernel, and it implements the parts that are genuinely testable as real code. The partition table parser, the boot config parser, and the ELF-like kernel image loader are not mock-ups. They parse real bytes, they enforce real invariants, and they are checked against a machine gate. What is simulated is the machine around them, the disk, the flat memory space, the processor mode state, and the act of jumping to an entry point. Ignition is a model, not a bootable artifact, and that line is drawn on purpose.

## The gap it fills

Most people learn how a computer boots from prose and diagrams. The interesting parts stay abstract. What does a loader actually do when it places a segment at a virtual address. How is the bss zeroed. What makes a partition table valid. When exactly does the CPU leave real mode. Ignition turns each of those questions into code you can read, run, and step through, with a correctness gate that proves the loader does what it claims.

## Quickstart

```
cargo run -- demo          # boot the bundled demo disk and print the whole chain
cargo run -- boot IMAGE    # boot a disk image file
cargo test                 # run the unit tests and the correctness gate
```

A demo run walks firmware, stage 1, stage 2, the partition table, the mini filesystem, the boot config, the kernel image load with per-segment logging, the memory map, the real-mode to protected-mode switch, and the hand-off to a stub kernel that proves it ran.

## API

The crate is a library plus a thin CLI. The pieces mirror the boot chain.

- `ignition::disk::Disk`, `ignition::memory::Memory`, `ignition::cpu::Cpu` make up `ignition::machine::Machine`.
- `ignition::partition::parse_mbr` parses and validates an MBR-style partition table.
- `ignition::config::parse_config` parses and validates the boot configuration text.
- `ignition::igfs::parse_superblock` reads the mini filesystem superblock.
- `ignition::elf` holds the kernel image format. `parse_header`, `parse_program_headers`, `validate_segments`, and `load` are the real parser and loader. `build_image` assembles an image, the inverse of `load`.
- `ignition::image::build_disk` and `build_demo_disk` assemble a full disk image.
- `ignition::boot::boot` and `boot_default` walk every stage and return a `BootReport` with the log, every parsed structure, the memory map, and the final machine state.

Minimal use:

```rust
use ignition::disk::Disk;
use ignition::image::build_demo_disk;
use ignition::boot::boot_default;

let disk = Disk::from_image(build_demo_disk()).unwrap();
let report = boot_default(disk).unwrap();
for line in &report.log {
    println!("{line}");
}
```

## The correctness gate

The gate lives in `tests/gate.rs` and in per-module unit tests. It proves three claims.

1. Load correctness. For random valid kernel images, after loading, every segment's bytes are present at its virtual address, the bss beyond the file size is zeroed, the entry point matches the header, and nothing outside a declared segment is written. A build then load then read-back round trip matches.
2. Validation and rejection. Malformed inputs are rejected rather than mis-loaded. A bad boot signature, a bad kernel magic, a segment that exceeds memory, overlapping segments, a memory size smaller than the file size, an entry point outside an executable segment, and a truncated image each produce a clear error and never a partial or corrupt load.
3. Boot sequence and determinism. The full chain completes in the correct order for a valid disk, the stub kernel runs after hand-off, and the same disk image always yields an identical log and memory map.

The fuzz loops are bounded for CI and controllable through environment variables. `IGNITION_FUZZ_OPS` sets the iteration count and `IGNITION_FUZZ_SEED` sets the starting seed, so runs stay fast and reproducible.

```
IGNITION_FUZZ_OPS=5000 cargo test gate1_load_correctness_and_roundtrip
```

## Design

The full walkthrough of the boot chain, the on-disk formats, the segment loader, the mode transition, and why each gate proves its claim is in [DESIGN.md](DESIGN.md).

## License

MIT.
