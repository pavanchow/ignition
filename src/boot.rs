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
use crate::igfs::{parse_superblock, SuperBlock};
use crate::image::{STAGE1_MAGIC, STAGE2_MAGIC};
use crate::kernel::{Handoff, Kernel, KernelOutcome, StubKernel};
use crate::machine::{
    Machine, DEFAULT_RAM_SIZE, KERNEL_OUTPUT_ADDR, LOW_RESERVED_END, MBR_LOAD_ADDR, MEMMAP_ADDR,
    STAGE2_LOAD_ADDR,
};
use crate::memmap::{MemoryMap, RegionKind};
use crate::partition::{parse_mbr, PartitionEntry, PartitionTable};

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

/// Boot a disk with the default stub kernel and default RAM size.
pub fn boot_default(disk: Disk) -> BootResult<BootReport> {
    boot(disk, &StubKernel, DEFAULT_RAM_SIZE)
}

/// Boot a disk with a specific kernel and RAM size, walking the whole chain.
pub fn boot(disk: Disk, kernel: &dyn Kernel, ram_size: usize) -> BootResult<BootReport> {
    let mut machine = Machine::new(disk, ram_size);
    let mut log = Vec::new();

    // Stage 0: firmware power on self test.
    machine.cpu = crate::cpu::Cpu::at_power_on();
    log.push(format!(
        "[post] firmware online, {} bytes RAM, {} disk sectors, CPU in real mode",
        machine.memory.size(),
        machine.disk.sector_count()
    ));

    // Stage 1: firmware loads the MBR boot sector to 0x7C00 and checks it.
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

    // Stage 1 reads its boot parameter block to find stage 2, then loads it.
    let stage2_lba = u64::from(read_u32(&sector0, 4).unwrap_or(0));
    let stage2_count = u64::from(read_u32(&sector0, 8).unwrap_or(0));
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
    let sb_bytes = machine
        .disk
        .read_bytes(part_byte_base, crate::igfs::SUPERBLOCK_SIZE as u64)?
        .to_vec();
    let superblock = parse_superblock(&sb_bytes)?;
    log.push(format!(
        "[igfs] superblock ok, config {} bytes, kernel {} bytes",
        superblock.config_len, superblock.kernel_len
    ));

    // Stage 2: read and parse the boot config.
    let config_bytes = machine
        .disk
        .read_bytes(
            part_byte_base + u64::from(superblock.config_offset),
            u64::from(superblock.config_len),
        )?
        .to_vec();
    let config_text = String::from_utf8(config_bytes)
        .map_err(|_| BootError::Config("config is not valid UTF-8".into()))?;
    let config = parse_config(&config_text)?;
    log.push(format!(
        "[config] kernel={} cmdline={:?} timeout={}",
        config.kernel, config.cmdline, config.timeout
    ));

    // Stage 2: read the kernel image bytes named by the config.
    let kernel_bytes = machine
        .disk
        .read_bytes(
            part_byte_base + u64::from(superblock.kernel_offset),
            u64::from(superblock.kernel_len),
        )?
        .to_vec();

    // Parse the header and program headers, then load segments into memory.
    let header = elf::parse_header(&kernel_bytes)?;
    let program_headers = elf::parse_program_headers(&kernel_bytes, &header)?;
    log.push(format!(
        "[elf] magic KIMG v{}, entry {:#x}, {} segment(s)",
        header.version, header.entry, header.phnum
    ));
    let loaded = elf::load(&kernel_bytes, &mut machine.memory)?;
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
        "[kernel] '{}' running, read {} memory map region(s), boot complete",
        outcome.marker, outcome.regions_seen
    ));

    Ok(BootReport {
        log,
        partition_table,
        boot_partition,
        superblock,
        config,
        header,
        program_headers,
        loaded,
        memory_map,
        machine,
        handoff,
        outcome,
    })
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
