//! The correctness gate for Ignition.
//!
//! Three properties, exactly as specified:
//! 1. Kernel image load correctness over random valid images.
//! 2. Validation and rejection of malformed inputs with no partial load.
//! 3. Full boot sequence ordering plus determinism.
//!
//! The fuzz loops are bounded and controllable through environment variables so
//! CI stays fast and reproducible: `IGNITION_FUZZ_OPS` sets the iteration count
//! and `IGNITION_FUZZ_SEED` sets the starting seed.

#![warn(clippy::pedantic)]
// Bounded arithmetic on small known values, casts are intentional in context.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]

use ignition::boot::{boot_default, BootSlot};
use ignition::disk::Disk;
use ignition::elf::{
    build_image, load, parse_header, parse_program_headers, SegmentSpec, FLAG_R, FLAG_W, FLAG_X,
    HEADER_SIZE, PROGRAM_HEADER_SIZE,
};
use ignition::error::{BootError, ElfError};
use ignition::image::{
    build_ab_demo_disk, build_demo_disk, build_disk, build_disk_ab, corrupt_kernel_image,
    demo_kernel_image,
};
use ignition::machine::LOADER_RESERVED_END;
use ignition::memory::Memory;

/// A tiny deterministic PRNG (xorshift64star) so runs are reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

fn fuzz_ops() -> u64 {
    std::env::var("IGNITION_FUZZ_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(400)
}

fn fuzz_seed() -> u64 {
    std::env::var("IGNITION_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x1234_5678_9ABC_DEF0)
}

const RAM: usize = 1 << 20;

/// Build a random valid set of non overlapping segments plus a valid entry.
fn random_segments(rng: &mut Rng) -> (u64, Vec<SegmentSpec>) {
    let nseg = rng.range(1, 5) as usize;
    let mut segs = Vec::new();
    let mut cursor = rng.below(4096);

    for i in 0..nseg {
        let gap = rng.below(512);
        let base = cursor + gap;
        let memsz = rng.range(1, 4096);
        if base + memsz + 4096 > RAM as u64 {
            break;
        }
        let filesz = rng.below(memsz + 1);
        let data: Vec<u8> = (0..filesz).map(|_| rng.below(256) as u8).collect();
        // Make the first segment executable so it can hold the entry point.
        let flags = if i == 0 { FLAG_R | FLAG_X } else { FLAG_R | FLAG_W };
        segs.push(SegmentSpec { vaddr: base, memsz: memsz as u32, flags, data });
        cursor = base + memsz;
    }

    // Entry falls inside the first (executable) segment.
    let first = &segs[0];
    let entry = first.vaddr + rng.below(u64::from(first.memsz));
    (entry, segs)
}

#[test]
fn gate1_load_correctness_and_roundtrip() {
    let mut rng = Rng::new(fuzz_seed());
    let ops = fuzz_ops();

    for iter in 0..ops {
        let (entry, segs) = random_segments(&mut rng);
        let image = build_image(entry, &segs);

        let mut mem = Memory::new(RAM);
        let loaded = load(&image, &mut mem).unwrap_or_else(|e| {
            panic!("iter {iter}: valid image rejected: {e}");
        });

        // Entry matches the header.
        assert_eq!(loaded.entry, entry, "iter {iter}: entry mismatch");
        assert_eq!(loaded.segments.len(), segs.len(), "iter {iter}: segment count");

        // Build a reference image of memory by applying only the declared writes.
        let mut reference = vec![0u8; RAM];
        for spec in &segs {
            let start = spec.vaddr as usize;
            reference[start..start + spec.data.len()].copy_from_slice(&spec.data);
            // bss stays zero, which the reference already is.
        }

        // Every byte of RAM matches: segment bytes present, bss zeroed, nothing
        // outside a declared segment written.
        let actual = mem.read(0, RAM).unwrap();
        assert!(actual == reference.as_slice(), "iter {iter}: memory image differs");

        // Explicit per segment checks and a read back round trip.
        for (spec, seg) in segs.iter().zip(loaded.segments.iter()) {
            let file_back = mem.read(seg.vaddr, spec.data.len()).unwrap();
            assert_eq!(file_back, spec.data.as_slice(), "iter {iter}: segment bytes");
            let bss = seg.bss_len() as usize;
            if bss > 0 {
                let z = mem.read(seg.vaddr + u64::from(seg.filesz), bss).unwrap();
                assert!(z.iter().all(|&b| b == 0), "iter {iter}: bss not zeroed");
            }
        }
    }
}

#[test]
fn gate2_reject_bad_magic() {
    let (entry, segs) = {
        let mut rng = Rng::new(1);
        random_segments(&mut rng)
    };
    let mut image = build_image(entry, &segs);
    image[0] = b'X';
    let mut mem = Memory::new(RAM);
    let err = load(&image, &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::BadMagic { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_truncated() {
    let image = build_image(
        0x1000,
        &[SegmentSpec { vaddr: 0x1000, memsz: 16, flags: FLAG_R | FLAG_X, data: vec![1, 2, 3, 4] }],
    );
    // Cut off part of the segment payload.
    let truncated = image[..image.len() - 2].to_vec();
    let mut mem = Memory::new(RAM);
    let err = load(&truncated, &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::SegmentFileRange { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_too_small() {
    let mut mem = Memory::new(RAM);
    let err = load(&[0u8; 4], &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::TooSmall { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_segment_out_of_range() {
    let image = build_image(
        (RAM as u64) - 4,
        &[SegmentSpec {
            vaddr: (RAM as u64) - 4,
            memsz: 64,
            flags: FLAG_R | FLAG_X,
            data: vec![1, 2, 3, 4],
        }],
    );
    let mut mem = Memory::new(RAM);
    let err = load(&image, &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::SegmentMemoryRange { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_overlapping_segments() {
    let segs = vec![
        SegmentSpec { vaddr: 0x2000, memsz: 0x1000, flags: FLAG_R | FLAG_X, data: vec![0xAA; 16] },
        SegmentSpec { vaddr: 0x2800, memsz: 0x1000, flags: FLAG_R | FLAG_W, data: vec![0xBB; 16] },
    ];
    let image = build_image(0x2000, &segs);
    let mut mem = Memory::new(RAM);
    let err = load(&image, &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::SegmentOverlap { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_mem_less_than_file() {
    // Hand craft a header whose memsz < filesz, which build_image never emits.
    let mut image = Vec::new();
    image.extend_from_slice(b"KIMG");
    image.extend_from_slice(&1u16.to_le_bytes()); // version
    image.extend_from_slice(&0u16.to_le_bytes()); // flags
    image.extend_from_slice(&0x1000u64.to_le_bytes()); // entry
    image.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes()); // phoff
    image.extend_from_slice(&1u16.to_le_bytes()); // phnum
    image.extend_from_slice(&0u16.to_le_bytes()); // pad
    // program header: offset, vaddr, filesz=8, memsz=4, flags
    let data_off = (HEADER_SIZE + PROGRAM_HEADER_SIZE) as u32;
    image.extend_from_slice(&data_off.to_le_bytes());
    image.extend_from_slice(&0x1000u64.to_le_bytes());
    image.extend_from_slice(&8u32.to_le_bytes());
    image.extend_from_slice(&4u32.to_le_bytes());
    image.extend_from_slice(&(FLAG_R | FLAG_X).to_le_bytes());
    image.extend_from_slice(&0u32.to_le_bytes());
    image.extend_from_slice(&[0u8; 8]);

    let mut mem = Memory::new(RAM);
    let err = load(&image, &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::MemLessThanFile { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_entry_not_executable() {
    let segs = vec![
        SegmentSpec { vaddr: 0x1000, memsz: 0x100, flags: FLAG_R | FLAG_W, data: vec![1, 2, 3] },
    ];
    let image = build_image(0x1000, &segs);
    let mut mem = Memory::new(RAM);
    let err = load(&image, &mut mem).unwrap_err();
    assert!(matches!(err, ElfError::EntryNotExecutable { .. }), "got {err}");
    assert_no_partial_load(&mem);
}

#[test]
fn gate2_reject_bad_boot_signature() {
    let mut image = build_demo_disk();
    // Corrupt the MBR boot signature.
    image[510] = 0;
    image[511] = 0;
    let disk = Disk::from_image(image).unwrap();
    let err = boot_default(disk).unwrap_err();
    assert!(matches!(err, BootError::BadBootSignature { .. }), "got {err}");
}

#[test]
fn gate3_boot_sequence_order() {
    let disk = Disk::from_image(build_demo_disk()).unwrap();
    let report = boot_default(disk).expect("demo must boot");

    // Stages appear in the correct order.
    let tags: Vec<&str> = report
        .log
        .iter()
        .map(|l| l.split(']').next().unwrap_or("").trim_start_matches('['))
        .collect();
    let expected_prefix = [
        "post", "stage1", "stage2", "parttab", "igfs", "config", "elf",
    ];
    for (i, tag) in expected_prefix.iter().enumerate() {
        assert_eq!(&tags[i], tag, "stage {i} out of order");
    }
    assert!(tags.contains(&"memmap"));
    assert!(tags.contains(&"cpu"));
    assert!(tags.contains(&"handoff"));
    assert!(tags.contains(&"kernel"));

    // The stub kernel ran after hand off.
    assert_eq!(report.outcome.marker, "IGNITED");
    assert_eq!(report.machine.cpu.mode, ignition::cpu::CpuMode::ProtectedMode);
    assert!(report.machine.cpu.a20);
    assert_eq!(report.machine.cpu.ip, report.header.entry);

    // The bss segment in the demo kernel was zero filled.
    let bss = report.loaded.segments.iter().find(|s| s.bss_len() > 0).unwrap();
    let z = report
        .machine
        .memory
        .read(bss.vaddr + u64::from(bss.filesz), bss.bss_len() as usize)
        .unwrap();
    assert!(z.iter().all(|&b| b == 0));
}

#[test]
fn gate3_determinism() {
    let a = boot_default(Disk::from_image(build_demo_disk()).unwrap()).unwrap();
    let b = boot_default(Disk::from_image(build_demo_disk()).unwrap()).unwrap();

    assert_eq!(a.log, b.log, "log differs between identical runs");
    assert_eq!(a.memory_map, b.memory_map, "memory map differs");
    let ram = a.machine.memory.size();
    assert!(
        a.machine.memory.read(0, ram).unwrap() == b.machine.memory.read(0, ram).unwrap(),
        "memory image differs between identical runs"
    );
}

#[test]
fn config_selects_kernel_and_boots() {
    let cfg = "kernel = /boot/custom\ncmdline = ro\ntimeout = 1\n";
    let disk = Disk::from_image(build_disk(cfg, &demo_kernel_image())).unwrap();
    let report = boot_default(disk).unwrap();
    assert_eq!(report.config.kernel, "/boot/custom");
    assert_eq!(report.config.cmdline, "ro");
    assert_eq!(report.config.timeout, 1);
}

fn assert_no_partial_load(mem: &Memory) {
    let all = mem.read(0, mem.size()).unwrap();
    assert!(all.iter().all(|&b| b == 0), "memory was partially written on a rejected image");
}

/// A kernel segment aimed at loader reserved low memory must be rejected before
/// anything is written, so the memory map can never claim a byte twice.
#[test]
fn gate_reject_kernel_in_reserved_memory() {
    for vaddr in [0u64, 0x7c00, 0x8000, 0x9000, LOADER_RESERVED_END - 1] {
        let seg = SegmentSpec { vaddr, memsz: 256, flags: FLAG_R | FLAG_X, data: vec![0xEE; 16] };
        let kernel = build_image(vaddr, &[seg]);
        let disk = Disk::from_image(build_disk("kernel = /k\n", &kernel)).unwrap();
        let err = boot_default(disk).unwrap_err();
        assert!(
            matches!(err, BootError::KernelPlacement { .. }),
            "segment at {vaddr:#x} should be rejected as placement, got {err}"
        );
    }
    // A segment exactly at the reserved boundary is allowed.
    let ok = SegmentSpec {
        vaddr: LOADER_RESERVED_END,
        memsz: 256,
        flags: FLAG_R | FLAG_X,
        data: vec![0xEE; 16],
    };
    let kernel = build_image(LOADER_RESERVED_END, &[ok]);
    let disk = Disk::from_image(build_disk("kernel = /k\n", &kernel)).unwrap();
    assert!(boot_default(disk).is_ok(), "segment at the reserved boundary must boot");
}

/// The flagship atomicity gate: when slot A is corrupt, the loader falls back to
/// slot B, and RAM must match an independent reference built from slot B's
/// headers exactly. Any byte left behind by the failed slot A load would make
/// this reference comparison fail, so a pass proves the rejection was atomic.
#[test]
fn gate_ab_fallback_is_atomic() {
    let disk = Disk::from_image(build_ab_demo_disk()).unwrap();
    let report = boot_default(disk).expect("ab demo must fall back and boot");
    assert_eq!(report.booted_slot, BootSlot::B, "should have booted the fallback slot");

    // Independent reference: only slot B's declared segment writes over zeroed RAM.
    let good = demo_kernel_image();
    let header = parse_header(&good).unwrap();
    let headers = parse_program_headers(&good, &header).unwrap();
    let ram = report.machine.memory.size();
    let mut reference = vec![0u8; ram];
    for h in &headers {
        let start = h.vaddr as usize;
        let file = &good[h.offset as usize..h.offset as usize + h.filesz as usize];
        reference[start..start + file.len()].copy_from_slice(file);
    }
    // The loader also writes the boot sector, stage 2, and the memory map into
    // reserved low memory, so compare only the region a kernel can occupy.
    let boundary = LOADER_RESERVED_END as usize;
    let actual = report.machine.memory.read(0, ram).unwrap();
    assert!(
        actual[boundary..] == reference[boundary..],
        "fallback RAM above the reserved boundary differs from slot B reference"
    );
}

/// When both slots are corrupt the boot fails cleanly with both errors named.
#[test]
fn gate_reject_when_all_slots_fail() {
    let disk = Disk::from_image(build_disk_ab(
        "kernel = /k\n",
        &corrupt_kernel_image(),
        Some(&corrupt_kernel_image()),
    ))
    .unwrap();
    let err = boot_default(disk).unwrap_err();
    assert!(matches!(err, BootError::AllSlotsFailed { .. }), "expected AllSlotsFailed, got {err}");
}

/// A single corrupt slot with no fallback surfaces the real underlying error.
#[test]
fn gate_reject_single_corrupt_slot() {
    let disk = Disk::from_image(build_disk("kernel = /k\n", &corrupt_kernel_image())).unwrap();
    let err = boot_default(disk).unwrap_err();
    assert!(matches!(err, BootError::Elf(ElfError::BadMagic { .. })), "got {err}");
}
