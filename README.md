<img src="docs/logo.svg" alt="Ignition logo" width="96">

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
cargo run -- ab-demo       # boot a disk whose A slot is corrupt and watch it fall back to B
cargo run -- boot IMAGE    # boot a disk image file
cargo test                 # run the unit tests, the correctness gate, and the stress harness
```

A demo run walks firmware, stage 1, stage 2, the partition table, the mini filesystem, the boot config, the kernel image load with per-segment logging, the memory map, the real-mode to protected-mode switch, and the hand-off to a stub kernel that proves it ran. The ab-demo run adds an A/B slot fallback, where a corrupt primary kernel is rejected atomically and the loader boots the fallback slot instead.

## API

The crate is a library plus a thin CLI. The pieces mirror the boot chain.

- `ignition::disk::Disk`, `ignition::memory::Memory`, `ignition::cpu::Cpu` make up `ignition::machine::Machine`.
- `ignition::partition::parse_mbr` parses and validates an MBR-style partition table.
- `ignition::config::parse_config` parses and validates the boot configuration text.
- `ignition::igfs::parse_superblock` reads the mini filesystem superblock, which names the config and two kernel slots, A and B, and validates every extent against the partition bounds.
- `ignition::elf` holds the kernel image format. `parse_header`, `parse_program_headers`, `validate_segments`, and `load` are the real parser and loader. `build_image` assembles an image, the inverse of `load`.
- `ignition::image::build_disk` builds a single-slot disk, `build_disk_ab` builds an A/B disk, and `build_demo_disk` and `build_ab_demo_disk` assemble the bundled demos.
- `ignition::boot::boot` and `boot_default` walk every stage and return a `BootReport` with the log, every parsed structure, the slot that booted, the memory map, and the final machine state.

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

The gate lives in `tests/gate.rs`, the bounded stress harness in `tests/stress.rs`, and focused unit tests in each module. Together they prove five claims.

1. Load correctness. For random valid kernel images, after loading, every segment's bytes are present at its virtual address, the bss beyond the file size is zeroed, the entry point matches the header, and nothing outside a declared segment is written. The whole RAM is compared byte for byte against an independent reference built from the parsed headers, not from the builder inputs.
2. Validation and rejection. Malformed inputs are rejected rather than mis-loaded. A bad boot signature, a bad kernel magic, a segment that exceeds memory, overlapping segments, a memory size smaller than the file size, an entry point outside an executable segment, a truncated image, and a kernel segment aimed at loader reserved memory each produce a clear error and never a partial or corrupt load. After every rejection the RAM is asserted to be entirely zero.
3. Boot sequence and determinism. The full chain completes in the correct order for a valid disk, the stub kernel runs after hand-off, and the same disk image always yields an identical log and memory map.
4. A/B fallback atomicity. When the primary kernel slot is corrupt, the loader falls back to slot B, and the RAM after fallback equals an independent reference for slot B. That equality can only hold if the failed slot A load wrote nothing, so it proves the rejection was atomic under the feature that depends on it.
5. Memory map soundness. Every successful boot produces a map whose regions are sorted, disjoint, and inside RAM, with the loader regions intact and every kernel segment covered by a matching kernel region.

The fuzz loops are bounded for CI and controllable through environment variables. `IGNITION_FUZZ_OPS` sets the iteration count and `IGNITION_FUZZ_SEED` sets the starting seed, so runs stay fast and reproducible.

```
IGNITION_FUZZ_OPS=5000 cargo test
```

## Design

The full walkthrough of the boot chain, the on-disk formats, the segment loader, the mode transition, and why each gate proves its claim is in [DESIGN.md](DESIGN.md).

## License

MIT.
