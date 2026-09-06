//! Bounded, deterministic stress harness for Ignition, a permanent gate member.
//!
//! Five phases, all reproducible under a seed:
//! A. Loader correctness over random layouts, compared against an independently
//!    constructed reference memory image built from the parsed headers.
//! B. Malformed image storms: every header field corrupted, truncations,
//!    overlaps, boundary entries. Every rejection must be clean and atomic and
//!    every acceptance must load exactly what its headers declare.
//! C. Full boot chain over random disks. Every successful boot must produce a
//!    memory map that is sorted, disjoint, and inside RAM, with the loader
//!    regions intact and every kernel segment covered.
//! D. A/B fallback storms: slot A is corrupted, slot B is good, and the RAM
//!    after fallback must equal an independent reference for slot B, which only
//!    holds if the failed slot A load wrote nothing.
//! E. Explicit u32/u64 boundary values in every field, nothing may panic.
//!
//! Scale is bounded and controllable so CI stays fast and the host disk stays
//! quiet: `IGNITION_FUZZ_OPS` sets the per phase iteration count and
//! `IGNITION_FUZZ_SEED` sets the starting seed. Structures are capped so no loop
//! can grow without bound.

#![warn(clippy::pedantic)]
// The harness does bounded arithmetic on small, known values, so the width casts
// are intentional and cannot overflow in context.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]

use ignition::boot::{boot_default, BootSlot};
use ignition::disk::Disk;
use ignition::elf::{
    build_image, load, parse_header, parse_program_headers, validate_segments, ProgramHeader,
    SegmentSpec, FLAG_R, FLAG_W, FLAG_X,
};
use ignition::error::BootError;
use ignition::image::{build_demo_disk, build_disk, build_disk_ab, demo_kernel_image};
use ignition::machine::{
    LOADER_RESERVED_END, LOW_RESERVED_END, MBR_LOAD_ADDR, MEMMAP_ADDR, STAGE2_LOAD_ADDR,
};
use ignition::memmap::{MemoryMap, RegionKind};
use ignition::memory::Memory;

/// A boundary case mutator applied to a freshly built image.
type Mutator = fn(&mut Vec<u8>);

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
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

fn fuzz_ops() -> u64 {
    std::env::var("IGNITION_FUZZ_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300)
}

fn fuzz_seed() -> u64 {
    std::env::var("IGNITION_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x1C7A_5EED_0000_0001)
}

// Bounded working sizes: a rejected image never allocates beyond these, and each
// iteration drops its Memory before the next, so peak use stays at one RAM.
const RAM_SIZES: [usize; 3] = [1 << 16, 1 << 18, 1 << 20];
const BOOT_RAM_SIZES: [usize; 2] = [1 << 20, 4 << 20];
const MAX_SEG_BYTES: u64 = 4096;

/// Random non overlapping segments packed into `ram`, first executable, entry
/// inside it. `min_base` is the lowest virtual address a segment may use.
fn random_segments(rng: &mut Rng, ram: u64, min_base: u64, max_segs: u64) -> (u64, Vec<SegmentSpec>) {
    let nseg = rng.range(1, max_segs) as usize;
    let mut segs = Vec::new();
    let mut cursor = min_base + rng.below((ram - min_base) / 2);
    for i in 0..nseg {
        let gap = rng.below(1024);
        let base = cursor + gap;
        let memsz = rng.range(1, MAX_SEG_BYTES);
        if base + memsz + 4096 > ram {
            break;
        }
        let filesz = rng.below(memsz + 1);
        let data: Vec<u8> = (0..filesz).map(|j| ((i as u64 * 31 + j) & 0xFF) as u8).collect();
        let flags = if i == 0 {
            FLAG_R | FLAG_X
        } else if rng.below(2) == 0 {
            FLAG_R | FLAG_W
        } else {
            FLAG_R
        };
        segs.push(SegmentSpec { vaddr: base, memsz: memsz as u32, flags, data });
        cursor = base + memsz;
    }
    let first = &segs[0];
    let entry = first.vaddr + rng.below(u64::from(first.memsz));
    (entry, segs)
}

/// Reference memory image built independently from the parsed headers.
fn reference_from_headers(image: &[u8], headers: &[ProgramHeader], ram: usize) -> Vec<u8> {
    let mut reference = vec![0u8; ram];
    for h in headers {
        let start = h.vaddr as usize;
        let file = &image[h.offset as usize..h.offset as usize + h.filesz as usize];
        reference[start..start + file.len()].copy_from_slice(file);
    }
    reference
}

/// Assert the memory map is sorted, disjoint, and inside RAM, that the encoded
/// form agrees with the region count, that the loader owned regions are present,
/// and that every loaded kernel segment is covered by a matching kernel region.
fn assert_map_invariants(map: &MemoryMap, loaded: &ignition::elf::LoadedKernel, ram: usize) {
    let ram = ram as u64;
    let mut prev_end = 0u64;
    for r in &map.regions {
        assert!(r.base >= prev_end, "map region at {:#x} overlaps previous end {prev_end:#x}", r.base);
        let end = r.base.checked_add(r.size).expect("region size overflow");
        assert!(end <= ram, "map region [{:#x}, {end:#x}) exceeds RAM {ram:#x}", r.base);
        prev_end = end;
    }

    let encoded = map.encode();
    assert_eq!(read_count(&encoded), map.len() as u32, "encoded region count disagrees");
    assert_eq!(encoded.len(), 4 + map.len() * 20, "encoded map size disagrees");

    let expected = [
        (0u64, LOW_RESERVED_END, RegionKind::Reserved),
        (MBR_LOAD_ADDR, 512, RegionKind::Loader),
        (STAGE2_LOAD_ADDR, 512, RegionKind::Loader),
        (MEMMAP_ADDR, encoded.len() as u64, RegionKind::MemoryMap),
    ];
    for (base, size, kind) in expected {
        assert!(
            map.regions.iter().any(|r| r.base == base && r.size == size && r.kind == kind),
            "map is missing the {kind:?} region at {base:#x} size {size}"
        );
    }

    for seg in &loaded.segments {
        assert!(
            map.regions.iter().any(|r| {
                r.kind == RegionKind::Kernel && r.base == seg.vaddr && r.size == u64::from(seg.memsz)
            }),
            "kernel segment at {:#x} not covered by a matching kernel region",
            seg.vaddr
        );
    }
}

fn read_count(encoded: &[u8]) -> u32 {
    u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]])
}

/// Phase A: load correctness over random layouts against an independent reference.
fn phase_a_correctness(ops: u64, seed: u64) {
    let mut rng = Rng::new(seed);
    for iter in 0..ops {
        let ram = *rng.pick(&RAM_SIZES);
        let (entry, segs) = random_segments(&mut rng, ram as u64, 0, 8);
        let image = build_image(entry, &segs);

        let header = parse_header(&image).unwrap();
        let headers = parse_program_headers(&image, &header)
            .unwrap_or_else(|e| panic!("A iter {iter} seed {seed}: built image unparseable: {e}"));
        validate_segments(&image, &headers, ram)
            .unwrap_or_else(|e| panic!("A iter {iter} seed {seed}: built image invalid: {e}"));

        let mut mem = Memory::new(ram);
        let loaded = load(&image, &mut mem)
            .unwrap_or_else(|e| panic!("A iter {iter} seed {seed}: valid image rejected: {e}"));

        assert_eq!(loaded.entry, entry, "A iter {iter}: entry mismatch");
        assert_eq!(loaded.segments.len(), headers.len(), "A iter {iter}: segment count");

        let reference = reference_from_headers(&image, &headers, ram);
        let actual = mem.read(0, ram).unwrap();
        assert!(actual == reference.as_slice(), "A iter {iter} seed {seed}: memory differs from reference");
    }
}

fn poke_u32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn poke_u64(image: &mut [u8], at: usize, value: u64) {
    image[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Phase B: malformed image storms. Every rejection must be clean and atomic,
/// every acceptance must match a reference rebuilt from the parsed headers.
fn phase_b_malformed(ops: u64, seed: u64) {
    let mut rng = Rng::new(seed ^ 0xB0B);
    let mut rejected = 0u64;
    let mut accepted = 0u64;
    for iter in 0..ops {
        let ram = *rng.pick(&RAM_SIZES);
        let (entry, segs) = random_segments(&mut rng, ram as u64, 0, 6);
        let mut image = build_image(entry, &segs);
        let phoff = 24usize;
        let image_len = image.len() as u32;
        match rng.below(10) {
            0 => {
                let at = rng.below((phoff + 28) as u64) as usize;
                image[at] ^= 1 << rng.below(8);
            }
            1 => poke_u32(&mut image, phoff, *rng.pick(&[u32::MAX, 0xFFFF_F000, 1 << 31])),
            2 => poke_u64(&mut image, phoff + 4, *rng.pick(&[u64::MAX, u64::MAX - 8, 1 << 63])),
            3 => poke_u32(&mut image, phoff + 12, *rng.pick(&[u32::MAX, image_len + 1])),
            4 => poke_u32(&mut image, phoff + 16, *rng.pick(&[u32::MAX, u32::MAX - 1])),
            5 => poke_u32(&mut image, 16, *rng.pick(&[u32::MAX, u32::MAX - 4, 1 << 31])),
            6 => poke_u64(&mut image, 8, *rng.pick(&[u64::MAX, ram as u64, ram as u64 - 1])),
            7 => {
                let cut = 1 + rng.below(image.len() as u64) as usize;
                image.truncate(cut);
            }
            8 => {
                let a = rng.below(ram as u64 / 4).max(8);
                let delta = rng.below(64) as i64 - rng.below(64) as i64;
                let b = (a as i64 + delta).max(1) as u64;
                let segs2 = vec![
                    SegmentSpec { vaddr: a, memsz: 128, flags: FLAG_R | FLAG_X, data: vec![1; 8] },
                    SegmentSpec { vaddr: b, memsz: 128, flags: FLAG_R | FLAG_W, data: vec![2; 8] },
                ];
                image = build_image(segs2[0].vaddr, &segs2);
            }
            _ => {
                image[20] = if rng.below(2) == 0 { 0 } else { 0xFF };
                image[21] = 0xFF;
            }
        }

        let mut mem = Memory::new(ram);
        match load(&image, &mut mem) {
            Ok(loaded) => {
                accepted += 1;
                let header = parse_header(&image).unwrap();
                let headers = parse_program_headers(&image, &header).unwrap();
                validate_segments(&image, &headers, ram).unwrap();
                let reference = reference_from_headers(&image, &headers, ram);
                let actual = mem.read(0, ram).unwrap();
                assert!(actual == reference.as_slice(), "B iter {iter} seed {seed}: accepted corrupt image loads wrong bytes");
                assert_eq!(loaded.entry, header.entry, "B iter {iter}: entry mismatch on accepted image");
            }
            Err(e) => {
                rejected += 1;
                let all = mem.read(0, ram).unwrap();
                assert!(all.iter().all(|&b| b == 0), "B iter {iter} seed {seed}: partial load on rejected image: {e}");
            }
        }
    }
    println!("phase B: {ops} images, {rejected} rejected, {accepted} accepted");
}

/// Phase C: full boot chain over random disks, map invariants on every success,
/// clean rejection on every failure.
fn phase_c_boot_chain(ops: u64, seed: u64) {
    let mut rng = Rng::new(seed ^ 0xC0C);
    let mut booted = 0u64;
    let mut rejected = 0u64;
    for iter in 0..ops {
        let ram = *rng.pick(&BOOT_RAM_SIZES);
        // Half the time aim segments at reserved low memory, which must be
        // rejected cleanly rather than mis-loaded.
        let aim_reserved = rng.below(2) == 0;
        let min_base = if aim_reserved { 0 } else { LOADER_RESERVED_END };
        let (entry, segs) = random_segments(&mut rng, ram as u64, min_base, 6);
        let kernel = build_image(entry, &segs);
        let mut disk_bytes = build_disk("kernel = /boot/vmignition\n", &kernel);

        // Occasionally corrupt a disk level structure instead.
        match rng.below(8) {
            0 => {
                let at = rng.below(512) as usize;
                disk_bytes[at] ^= 1 << rng.below(8);
            }
            1 => {
                let at = 2 * 512 + rng.below(32) as usize;
                disk_bytes[at] ^= 1 << rng.below(8);
            }
            _ => {}
        }

        let Ok(disk) = Disk::from_image(disk_bytes) else {
            continue;
        };
        match boot_default(disk) {
            Ok(report) => {
                booted += 1;
                assert_map_invariants(&report.memory_map, &report.loaded, report.machine.memory.size());
                assert_eq!(
                    report.outcome.regions_seen as usize,
                    report.memory_map.len(),
                    "C iter {iter} seed {seed}: kernel saw a different region count"
                );
                assert_eq!(report.loaded.entry, report.header.entry, "C iter {iter}: entry disagree");
                assert_eq!(report.booted_slot, BootSlot::A, "C iter {iter}: single slot should boot A");
            }
            Err(e) => {
                rejected += 1;
                assert!(!format!("{e}").is_empty(), "C iter {iter}: empty error");
            }
        }
    }
    println!("phase C: {ops} disks, {booted} booted, {rejected} rejected");
}

/// Phase D: A/B fallback storms. Slot A is corrupted every way, slot B is good.
/// After fallback the RAM above the reserved boundary must equal an independent
/// reference for slot B, proving the failed slot A load was atomic.
fn phase_d_ab_fallback(ops: u64, seed: u64) {
    let mut rng = Rng::new(seed ^ 0xD0D);
    let good = demo_kernel_image();
    let good_header = parse_header(&good).unwrap();
    let good_headers = parse_program_headers(&good, &good_header).unwrap();
    let mut fell_back = 0u64;
    for iter in 0..ops {
        // Build a corrupt slot A image a few different ways.
        let mut bad = good.clone();
        match rng.below(4) {
            0 => bad[0] = b'X',                      // bad magic
            1 => poke_u32(&mut bad, 8, u32::MAX),    // wild entry low bits
            2 => bad.truncate(1 + rng.below(20) as usize), // truncated header
            _ => bad[20] = 0,                        // phnum zero
        }

        let disk = Disk::from_image(build_disk_ab("kernel = /boot/vmignition\n", &bad, Some(&good)))
            .unwrap();
        let report = boot_default(disk)
            .unwrap_or_else(|e| panic!("D iter {iter} seed {seed}: fallback boot failed: {e}"));
        assert_eq!(report.booted_slot, BootSlot::B, "D iter {iter}: should boot slot B");
        fell_back += 1;

        let ram = report.machine.memory.size();
        let reference = reference_from_headers(&good, &good_headers, ram);
        let boundary = LOADER_RESERVED_END as usize;
        let actual = report.machine.memory.read(0, ram).unwrap();
        assert!(
            actual[boundary..] == reference[boundary..],
            "D iter {iter} seed {seed}: fallback RAM differs from slot B reference (slot A not atomic)"
        );
    }
    println!("phase D: {ops} disks, {fell_back} fell back A to B cleanly");
}

/// Phase E: explicit boundary values in every field, nothing may panic.
fn phase_e_boundaries() {
    let ram = 1usize << 20;
    let base_segs = [SegmentSpec { vaddr: 0x1000, memsz: 32, flags: FLAG_R | FLAG_X, data: vec![7; 8] }];
    let mutators: [(&str, Mutator); 10] = [
        ("phnum max", |img| { img[20] = 0xFF; img[21] = 0xFF; }),
        ("phnum zero", |img| { img[20] = 0; img[21] = 0; }),
        ("phoff u32 max", |img| poke_u32(img, 16, u32::MAX)),
        ("entry u64 max", |img| poke_u64(img, 8, u64::MAX)),
        ("entry ram", |img| poke_u64(img, 8, 1u64 << 20)),
        ("p_offset u32 max", |img| poke_u32(img, 24, u32::MAX)),
        ("p_vaddr u64 max", |img| poke_u64(img, 28, u64::MAX)),
        ("p_filesz u32 max", |img| poke_u32(img, 36, u32::MAX)),
        ("p_memsz u32 max", |img| poke_u32(img, 40, u32::MAX)),
        ("version 9999", |img| { img[4] = 0x0F; img[5] = 0x27; }),
    ];

    let mut clean = 0u64;
    for (name, mutate) in mutators {
        let mut img = build_image(0x1000, &base_segs);
        mutate(&mut img);
        let mut mem = Memory::new(ram);
        match load(&img, &mut mem) {
            Ok(_) => panic!("E: boundary case '{name}' was accepted"),
            Err(e) => {
                clean += 1;
                let all = mem.read(0, ram).unwrap();
                assert!(all.iter().all(|&b| b == 0), "E: '{name}' partial load: {e}");
            }
        }
    }

    // Overlapping file ranges are legal: both segments must land correctly.
    let segs = vec![
        SegmentSpec { vaddr: 0x2000, memsz: 16, flags: FLAG_R | FLAG_X, data: vec![0xAA; 16] },
        SegmentSpec { vaddr: 0x3000, memsz: 16, flags: FLAG_R | FLAG_W, data: vec![0xBB; 16] },
    ];
    let mut img = build_image(0x2000, &segs);
    poke_u32(&mut img, 24 + 28, 80); // point second segment at the first's file bytes
    let mut mem = Memory::new(ram);
    let loaded = load(&img, &mut mem).unwrap();
    assert_eq!(loaded.segments.len(), 2, "E: shared file range rejected");
    assert_eq!(mem.read(0x2000, 16).unwrap(), &[0xAA; 16], "E: shared range first segment");
    assert_eq!(mem.read(0x3000, 16).unwrap(), &[0xAA; 16], "E: shared range second segment");

    // The demo disk must satisfy every map invariant.
    let demo = boot_default(Disk::from_image(build_demo_disk()).unwrap()).expect("demo must boot");
    assert_map_invariants(&demo.memory_map, &demo.loaded, demo.machine.memory.size());

    // A direct loader placement error class exists at the boot layer: confirm a
    // segment in reserved memory is rejected, not mis-loaded.
    let bad = build_image(MEMMAP_ADDR, &[SegmentSpec {
        vaddr: MEMMAP_ADDR,
        memsz: 64,
        flags: FLAG_R | FLAG_X,
        data: vec![0xEE; 16],
    }]);
    let err = boot_default(Disk::from_image(build_disk("kernel = /k\n", &bad)).unwrap()).unwrap_err();
    assert!(matches!(err, BootError::KernelPlacement { .. }), "E: reserved placement not rejected: {err}");

    println!("phase E: {clean} boundary cases rejected cleanly, shared file range loads, placement enforced");
}

#[test]
fn stress_all_phases() {
    let ops = fuzz_ops();
    let seed = fuzz_seed();
    let started = std::time::Instant::now();
    phase_a_correctness(ops, seed);
    phase_b_malformed(ops, seed);
    phase_c_boot_chain(ops / 2 + 1, seed);
    phase_d_ab_fallback(ops / 4 + 1, seed);
    phase_e_boundaries();
    println!("stress done: ops {ops} seed {seed:#x} in {:?}", started.elapsed());
}
