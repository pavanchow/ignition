//! The boot chain orchestrator: firmware, stage 1, stage 2, partition table,
//! filesystem, config, kernel load, memory map, mode switch, and hand off.
//!
//! Every stage appends to a deterministic log so the same disk image always
//! yields the same transcript and the same memory map.

use crate::bytes::read_u32;
use crate::config::{parse_config, BootConfig};
use crate::disk::{Disk, SECTOR_SIZE};
use crate::elf::{self, KernelHeader, LoadedKernel, ProgramHeader};
use crate::error::{BootError, BootResult};
use crate::igfs::{parse_superblock, FileExtent, SuperBlock};
use crate::image::{STAGE1_MAGIC, STAGE2_MAGIC};
use crate::kernel::{Handoff, Kernel, KernelOutcome, StubKernel};
use crate::machine::{
    Machine, DEFAULT_RAM_SIZE, KERNEL_OUTPUT_ADDR, LOADER_RESERVED_END, LOW_RESERVED_END,
    MBR_LOAD_ADDR, MEMMAP_ADDR, STAGE2_LOAD_ADDR,
};
use crate::memmap::{MemoryMap, RegionKind};
use crate::partition::{parse_mbr, PartitionEntry, PartitionTable};

/// Which kernel slot the loader booted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootSlot {
    /// The primary slot.
    A,
    /// The fallback slot, reached after slot A failed to load.
    B,
}

impl BootSlot {
    /// A short label for logging.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            BootSlot::A => "A",
            BootSlot::B => "B",
        }
    }
}

/// Everything the boot produced, for printing and for tests.
#[derive(Clone, Debug)]
pub struct BootReport {
    /// The stage by stage transcript.
    pub log: Vec<String>,
    /// The parsed partition table.
    pub partition_table: PartitionTable,
    /// The partition selected for boot.
    pub boot_partition: PartitionEntry,
    /// The filesystem superblock.
    pub superblock: SuperBlock,
    /// The parsed boot config.
    pub config: BootConfig,
    /// The slot the loader actually booted.
    pub booted_slot: BootSlot,
    /// The parsed kernel header.
    pub header: KernelHeader,
    /// The parsed program headers.
    pub program_headers: Vec<ProgramHeader>,
    /// The kernel after loading.
    pub loaded: LoadedKernel,
    /// The memory map handed to the kernel.
    pub memory_map: MemoryMap,
    /// The machine state after the run.
    pub machine: Machine,
    /// The hand off state.
    pub handoff: Handoff,
    /// What the kernel produced.
    pub outcome: KernelOutcome,
}

/// A kernel image successfully parsed, placed, and loaded from one slot.
struct LoadedSlot {
    slot: BootSlot,
    header: KernelHeader,
    program_headers: Vec<ProgramHeader>,
    loaded: LoadedKernel,
}

/// Boot a disk with the default stub kernel and default RAM size.
///
/// # Errors
/// Returns a [`BootError`] if any stage of the boot chain fails.
pub fn boot_default(disk: Disk) -> BootResult<BootReport> {
    boot(disk, &StubKernel, DEFAULT_RAM_SIZE)
}

/// Boot a disk with a specific kernel and RAM size, walking the whole chain.
///
/// # Errors
/// Returns a [`BootError`] if any stage fails: a bad boot signature or stage
/// magic, a malformed partition table, filesystem, or config, a kernel image
/// that cannot be loaded in either slot, or a failed hand off.
pub fn boot(disk: Disk, kernel: &dyn Kernel, ram_size: usize) -> BootResult<BootReport> {
    let mut machine = Machine::new(disk, ram_size);
    let mut log = Vec::new();

    let sector0 = firmware_and_stage1(&mut machine, &mut log)?;
    load_stage2(&mut machine, &sector0, &mut log)?;

    // Stage 2: read and validate the partition table.
    let partition_table = parse_mbr(&sector0, machine.disk.sector_count())?;
    let boot_partition = partition_table
        .boot_partition()
        .ok_or(BootError::NoBootablePartition)?;
    log.push(format!(
        "[parttab] bootable partition type {:#04x} at LBA {}, {} sectors",
        boot_partition.part_type, boot_partition.start_lba, boot_partition.sector_count
    ));

    // Stage 2: read the filesystem superblock at the partition start.
    let part_byte_base = u64::from(boot_partition.start_lba) * SECTOR_SIZE as u64;
    let partition_len = u64::from(boot_partition.sector_count) * SECTOR_SIZE as u64;
    let sb_bytes = machine
        .disk
        .read_bytes(part_byte_base, crate::igfs::SUPERBLOCK_SIZE as u64)?
        .to_vec();
    let superblock = parse_superblock(&sb_bytes, partition_len)?;
    log.push(format!(
        "[igfs] superblock ok, config {} bytes, slot A {} bytes, slot B {}",
        superblock.config.len,
        superblock.kernel_a.len,
        if superblock.has_slot_b() {
            format!("{} bytes", superblock.kernel_b.len)
        } else {
            "absent".to_string()
        }
    ));

    // Stage 2: read and parse the boot config.
    let config = read_config(&machine, part_byte_base, superblock.config, &mut log)?;

    // Stage 2: load the kernel from slot A, falling back to slot B on failure.
    let slot = load_kernel_slots(&mut machine, part_byte_base, &superblock, &mut log)?;
    let LoadedSlot { slot: booted_slot, header, program_headers, loaded } = slot;

    // Build the memory map and write it to RAM for the kernel.
    let memory_map = build_memory_map(&loaded, machine.memory.size());
    let encoded = memory_map.encode();
    machine.memory.write(MEMMAP_ADDR, &encoded)?;
    log.push(format!(
        "[memmap] {} regions written to {MEMMAP_ADDR:#x} ({} bytes)",
        memory_map.len(),
        encoded.len()
    ));

    // Real mode to protected mode transition.
    machine.cpu.enable_a20();
    machine.cpu.enter_protected_mode()?;
    log.push("[cpu] A20 enabled, entered protected mode".to_string());

    // Hand off: jump to the entry point and run the kernel.
    let handoff = Handoff { entry: loaded.entry, memmap_addr: MEMMAP_ADDR };
    machine.cpu.ip = handoff.entry;
    log.push(format!(
        "[handoff] jump to entry {:#x} in protected mode",
        handoff.entry
    ));
    let outcome = kernel.run(&mut machine, &handoff)?;
    // Confirm the kernel actually ran by reading back its marker.
    let marker = crate::kernel::read_marker(&machine, KERNEL_OUTPUT_ADDR)?;
    if marker != crate::kernel::KERNEL_OK_MARKER {
        return Err(BootError::Handoff("kernel did not signal a successful start".into()));
    }
    log.push(format!(
        "[kernel] '{}' running from slot {}, read {} memory map region(s), boot complete",
        outcome.marker,
        booted_slot.label(),
        outcome.regions_seen
    ));

    Ok(BootReport {
        log,
        partition_table,
        boot_partition,
        superblock,
        config,
        booted_slot,
        header,
        program_headers,
        loaded,
        memory_map,
        machine,
        handoff,
        outcome,
    })
}

/// Stage 0 and stage 1: POST, then load and check the MBR boot sector.
fn firmware_and_stage1(machine: &mut Machine, log: &mut Vec<String>) -> BootResult<Vec<u8>> {
    machine.cpu = crate::cpu::Cpu::at_power_on();
    log.push(format!(
        "[post] firmware online, {} bytes RAM, {} disk sectors, CPU in real mode",
        machine.memory.size(),
        machine.disk.sector_count()
    ));

    let sector0 = machine.disk.read_sector(0)?.to_vec();
    machine.memory.write(MBR_LOAD_ADDR, &sector0)?;
    if sector0[510..512] != crate::partition::MBR_SIGNATURE {
        return Err(BootError::BadBootSignature { sector: 0 });
    }
    if sector0[0..4] != STAGE1_MAGIC {
        return Err(BootError::BadStageMagic { stage: 1 });
    }
    log.push(format!(
        "[stage1] boot sector loaded at {MBR_LOAD_ADDR:#x}, signature 0x55AA ok"
    ));
    Ok(sector0)
}

/// Stage 1 reads its boot parameter block, then loads and checks stage 2.
fn load_stage2(machine: &mut Machine, sector0: &[u8], log: &mut Vec<String>) -> BootResult<()> {
    let stage2_lba = u64::from(read_u32(sector0, 4).unwrap_or(0));
    let stage2_count = u64::from(read_u32(sector0, 8).unwrap_or(0));
    if stage2_count == 0 {
        return Err(BootError::Post("stage 2 sector count is zero".into()));
    }
    let stage2 = machine.disk.read_sectors(stage2_lba, stage2_count)?.to_vec();
    machine.memory.write(STAGE2_LOAD_ADDR, &stage2)?;
    if stage2[0..4] != STAGE2_MAGIC {
        return Err(BootError::BadStageMagic { stage: 2 });
    }
    log.push(format!(
        "[stage2] loaded {stage2_count} sector(s) from LBA {stage2_lba} to {STAGE2_LOAD_ADDR:#x}"
    ));
    Ok(())
}

/// Read and parse the boot config named by the superblock.
fn read_config(
    machine: &Machine,
    part_byte_base: u64,
    config: FileExtent,
    log: &mut Vec<String>,
) -> BootResult<BootConfig> {
    let config_bytes = machine
        .disk
        .read_bytes(part_byte_base + u64::from(config.offset), u64::from(config.len))?
        .to_vec();
    let config_text = String::from_utf8(config_bytes)
        .map_err(|_| BootError::Config("config is not valid UTF-8".into()))?;
    let parsed = parse_config(&config_text)?;
    log.push(format!(
        "[config] kernel={} cmdline={:?} timeout={}",
        parsed.kernel, parsed.cmdline, parsed.timeout
    ));
    Ok(parsed)
}

/// Try to load slot A, then slot B on failure, into a freshly clean memory.
fn load_kernel_slots(
    machine: &mut Machine,
    part_byte_base: u64,
    superblock: &SuperBlock,
    log: &mut Vec<String>,
) -> BootResult<LoadedSlot> {
    let err_a = match try_load_slot(machine, part_byte_base, superblock.kernel_a, BootSlot::A, log) {
        Ok(slot) => return Ok(slot),
        Err(e) => e,
    };
    // A single slot disk surfaces the real error directly.
    if !superblock.has_slot_b() {
        return Err(err_a);
    }
    log.push(format!("[slot] slot A failed to load ({err_a}), trying slot B"));

    // Slot A's load is atomic, so RAM is still clean for slot B.
    match try_load_slot(machine, part_byte_base, superblock.kernel_b, BootSlot::B, log) {
        Ok(slot) => Ok(slot),
        Err(err_b) => Err(BootError::AllSlotsFailed {
            slot_a: err_a.to_string(),
            slot_b: err_b.to_string(),
        }),
    }
}

/// Parse, placement check, and load one kernel slot into memory.
fn try_load_slot(
    machine: &mut Machine,
    part_byte_base: u64,
    extent: FileExtent,
    slot: BootSlot,
    log: &mut Vec<String>,
) -> BootResult<LoadedSlot> {
    let kernel_bytes = machine
        .disk
        .read_bytes(part_byte_base + u64::from(extent.offset), u64::from(extent.len))?
        .to_vec();

    let header = elf::parse_header(&kernel_bytes)?;
    let program_headers = elf::parse_program_headers(&kernel_bytes, &header)?;

    // Placement policy: no kernel segment may land on loader reserved memory,
    // and the map that will describe this many segments must fit the scratch
    // area. Both checks run before any RAM is written so a rejected slot leaves
    // memory untouched for the fallback slot.
    check_placement(&program_headers, LOADER_RESERVED_END)?;
    check_loader_capacity(program_headers.len())?;

    let loaded = elf::load(&kernel_bytes, &mut machine.memory)?;
    log.push(format!(
        "[elf] slot {} KIMG v{}, entry {:#x}, {} segment(s)",
        slot.label(),
        header.version,
        header.entry,
        header.phnum
    ));
    for (i, seg) in loaded.segments.iter().enumerate() {
        log.push(format!(
            "[load] segment {i}: {} bytes file + {} bytes bss at {:#x} [{}]",
            seg.filesz,
            seg.bss_len(),
            seg.vaddr,
            elf::ProgramHeader {
                offset: 0,
                vaddr: seg.vaddr,
                filesz: seg.filesz,
                memsz: seg.memsz,
                flags: seg.flags,
            }
            .perm_string()
        ));
    }
    Ok(LoadedSlot { slot, header, program_headers, loaded })
}

/// Reject any segment that intersects the loader reserved low memory.
fn check_placement(headers: &[ProgramHeader], reserved_end: u64) -> BootResult<()> {
    for h in headers {
        // The reserved region starts at address zero, so a segment intersects it
        // exactly when it begins below `reserved_end`.
        if h.vaddr < reserved_end {
            return Err(BootError::KernelPlacement {
                vaddr: h.vaddr,
                memsz: h.memsz,
                reserved_end,
            });
        }
    }
    Ok(())
}

/// Reject an image with more segments than the loader scratch area can map.
fn check_loader_capacity(nseg: usize) -> BootResult<()> {
    // Reserved, two loader, memory map, one per segment, and one free region.
    let region_count = (3 + 1 + nseg + 1) as u64;
    let needed = 4 + region_count * 20;
    let available = KERNEL_OUTPUT_ADDR - MEMMAP_ADDR;
    if needed > available {
        return Err(BootError::LoaderCapacity { needed, available });
    }
    Ok(())
}

fn build_memory_map(loaded: &LoadedKernel, ram: usize) -> MemoryMap {
    let mut map = MemoryMap::new();
    map.add(0, LOW_RESERVED_END, RegionKind::Reserved);
    map.add(MBR_LOAD_ADDR, SECTOR_SIZE as u64, RegionKind::Loader);
    map.add(STAGE2_LOAD_ADDR, SECTOR_SIZE as u64, RegionKind::Loader);

    // The memory map region has a fixed size once the region count is known.
    let region_count = 3 + 1 + loaded.segments.len() + 1;
    let mm_size = 4 + region_count as u64 * 20;
    map.add(MEMMAP_ADDR, mm_size, RegionKind::MemoryMap);

    let mut highest = 0u64;
    for seg in &loaded.segments {
        map.add(seg.vaddr, u64::from(seg.memsz), RegionKind::Kernel);
        highest = highest.max(seg.vaddr + u64::from(seg.memsz));
    }

    // One free region from just past the kernel to the top of RAM.
    let free_base = round_up_u64(highest.max(MEMMAP_ADDR + mm_size), 0x1000);
    if free_base < ram as u64 {
        map.add(free_base, ram as u64 - free_base, RegionKind::Free);
    }
    map
}

fn round_up_u64(value: u64, multiple: u64) -> u64 {
    value.div_ceil(multiple) * multiple
}
