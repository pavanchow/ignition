# Ignition design

This document explains the boot chain Ignition models, the on-disk formats it parses, the segment loader at its core, the mode transition and hand-off, and why each part of the correctness gate proves what it claims.

A note on scope. Ignition is a deterministic model in safe std Rust. It implements real parsers and a real loader against a simulated machine. It does not execute machine code and it does not run on hardware. Everything below describes the model and states plainly where the model stops.

## The simulated machine

Three small pieces make up the machine.

- `Disk` is a read-only block device made of 512 byte sectors backed by a byte vector. It refuses images that are not a positive whole number of sectors, and every read is range checked.
- `Memory` is a flat, byte-addressable address space. Kernel virtual addresses map directly onto offsets in this space, an identity mapping that keeps the model simple while still proving the loader writes the right bytes to the right place. Writes, zero-fills, and reads are all range checked.
- `Cpu` holds the execution mode, the A20 line state, and the instruction pointer. It starts in real mode with A20 masked, which is how a real x86 machine powers on.

## The boot chain

The orchestrator in `boot.rs` walks the chain in order and appends one log line per step, so the transcript reads top to bottom like a real boot.

1. Power-on self test. Firmware comes online and sizes RAM and the disk. The CPU is in real mode.
2. Stage 1. Firmware copies sector 0, the master boot record, to the classic load address 0x7C00 and checks the 0x55AA boot signature. Stage 1 carries a small boot parameter block naming where stage 2 lives.
3. Stage 2. Stage 1 reads that parameter block and loads the stage 2 blob into memory. Each stage blob carries a magic marker so a corrupt or missing stage is caught.
4. Partition table. Stage 2 parses the MBR partition table and selects the bootable partition.
5. Filesystem. Stage 2 reads a mini filesystem superblock at the start of the partition. The superblock records where the boot config and the kernel image live.
6. Boot config. Stage 2 reads and parses the config text, which names the kernel path and its options.
7. Kernel load. Stage 2 reads the kernel image, parses the header and the program headers, then loads each segment into memory and zero-fills the bss.
8. Memory map. The loader builds a sorted memory map describing reserved, loader, kernel, memory-map, and free regions, and writes it into RAM for the kernel to read.
9. Mode switch. The loader enables the A20 line and moves the CPU from real mode to protected mode.
10. Hand-off. The loader confirms the entry point sits inside an executable segment, sets the instruction pointer, and jumps. A stub kernel runs, reads the memory map, and writes a proof-of-life marker back into RAM.

## On-disk formats

### Partition table, MBR style

The master boot record is a 512 byte sector. Its last two bytes are the 0x55AA signature. Four 16 byte partition entries begin at offset 446. Each entry carries a status byte, where 0x80 means bootable, a type byte, a start LBA, and a sector count. The legacy CHS fields are present in the layout and intentionally ignored.

Validation rejects a sector of the wrong size, a missing signature, a status byte that is neither zero nor 0x80, a partition that starts inside the MBR sector, a partition that runs past the end of the disk, and any two live partitions that overlap. A malformed table is an error, never a guess.

### Boot config, text

The config is a small key equals value text format, one directive per line, with hash comments. The `kernel` key is required and must be an absolute path. The `cmdline` and `timeout` keys are optional. Parsing rejects an unknown key, a missing equals sign, an empty value, a non-numeric timeout, a relative kernel path, and a missing kernel key.

### Mini filesystem superblock

So stage 2 can find files the way a real second stage reads a filesystem, the partition begins with a 24 byte superblock. It carries a magic, a version, and the byte offset and length of both the config and the kernel image within the partition. Parsing rejects a bad magic, an unsupported version, a short input, and a zero-length file.

### Kernel image, ELF-like

The kernel image is a deliberate, faithful cousin of ELF, small enough to read in one sitting. All integers are little endian.

The 24 byte header holds a KIMG magic, a version, a 64 bit entry point, the file offset of the program header table, and the number of program headers.

Each 28 byte program header holds a file offset, a destination virtual address, a file size, a memory size, and permission flags. The memory size is at least the file size, and the difference is bss.

## The segment loader

The loader is the testable core. It runs in a strict order so a rejected image never touches memory.

1. Parse the header. Reject a too-small image, a bad magic, an unsupported version, and a header that declares no segments.
2. Parse the program header table. Reject a table that lies outside the image bytes.
3. Validate every segment before writing anything. Reject a memory size smaller than the file size, file bytes that lie outside the image, a memory range that does not fit in RAM, and any two segments whose virtual ranges overlap.
4. Confirm the entry point sits inside an executable segment. Reject it otherwise.
5. Only now mutate memory. For each segment, copy the file bytes to the virtual address, then zero-fill the bss region beyond the file size. Record the loaded segments and the entry point.

Because all validation and the entry check happen before the first write, a rejected image leaves memory exactly as it was. The gate checks that property directly.

## The mode transition and hand-off

A real machine starts in 16 bit real mode with only the low megabyte reachable because the A20 line is masked. Moving to a modern kernel means enabling A20 and setting the protection enable bit to reach 32 bit protected mode. Ignition models this as explicit, checkable state. The switch fails if A20 is still masked and it fails if the machine is already in protected mode, so the ordering is enforced rather than assumed.

The hand-off then writes the memory map into RAM, sets the instruction pointer to the entry point, and runs the kernel. Because a simulator in safe Rust cannot execute the raw bytes of the loaded image, the loaded kernel is represented at run time by a small Rust implementation, and the jump is modeled as a call into it. The stub kernel reads the region count from the memory map the loader left and writes an IGNITED marker back into RAM. The boot then reads that marker back to confirm the hand-off actually reached running kernel code. This is the one place the model stands in for hardware, and it is called out rather than hidden.

## Why each gate proves its claim

The gate has three parts, in `tests/gate.rs`, plus focused unit tests in each module.

Gate 1, load correctness. It builds random valid images with non-overlapping segments and a valid entry, loads each into fresh RAM, and then compares the entire address space against a reference built only from the declared writes. Equality of the whole space proves three things at once. Segment bytes landed at the right address, the bss is zeroed, and nothing outside a declared segment was touched. A per-segment read-back confirms the round trip from build to load to read matches.

Gate 2, validation and rejection. It constructs one malformed input per failure mode, a bad boot signature, a bad kernel magic, a segment that exceeds memory, overlapping segments, a memory size smaller than the file size, an entry outside an executable segment, and a truncated image. For each it asserts the exact error variant and asserts that memory is still entirely zero, which proves no partial load occurred.

Gate 3, boot sequence and determinism. It boots the demo disk and asserts the stages appear in the correct order, the stub kernel ran, the CPU ended in protected mode with A20 enabled and the instruction pointer at the entry point, and the demo bss segment is zeroed. It then boots two identical disks and asserts the logs, the memory maps, and the full memory images are byte-for-byte equal, which proves the run is deterministic.

The fuzz iteration count and seed come from `IGNITION_FUZZ_OPS` and `IGNITION_FUZZ_SEED`, so CI stays bounded and every run is reproducible.
