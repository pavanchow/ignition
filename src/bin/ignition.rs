//! The Ignition command line tool: boot a disk image and print the whole chain.

use std::process::ExitCode;

use ignition::boot::{boot_default, BootReport};
use ignition::disk::Disk;
use ignition::image::build_demo_disk;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("help");

    match command {
        "demo" => run_boot(build_demo_disk()),
        "boot" => match args.get(2) {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => run_boot(bytes),
                Err(e) => {
                    eprintln!("error: cannot read '{path}': {e}");
                    ExitCode::FAILURE
                }
            },
            None => run_boot(build_demo_disk()),
        },
        "help" | "-h" | "--help" => {
            print_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown command '{other}'\n");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn run_boot(image: Vec<u8>) -> ExitCode {
    let disk = match Disk::from_image(image) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match boot_default(disk) {
        Ok(report) => {
            print_report(&report);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("boot failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_report(report: &BootReport) {
    println!("== boot chain ==");
    for line in &report.log {
        println!("{line}");
    }

    println!("\n== partition table ==");
    for (i, e) in report.partition_table.entries.iter().enumerate() {
        if e.is_empty() {
            println!("  [{i}] <empty>");
        } else {
            println!(
                "  [{i}] {}type {:#04x} start {} count {}",
                if e.bootable { "boot " } else { "     " },
                e.part_type,
                e.start_lba,
                e.sector_count
            );
        }
    }

    println!("\n== boot config ==");
    println!("  kernel  = {}", report.config.kernel);
    println!("  cmdline = {}", report.config.cmdline);
    println!("  timeout = {}", report.config.timeout);

    println!("\n== kernel image header ==");
    println!("  magic   = KIMG v{}", report.header.version);
    println!("  entry   = {:#x}", report.header.entry);
    println!("  segments= {}", report.header.phnum);
    for (i, ph) in report.program_headers.iter().enumerate() {
        println!(
            "  ph[{i}]  vaddr {:#x} filesz {} memsz {} [{}]",
            ph.vaddr,
            ph.filesz,
            ph.memsz,
            ph.perm_string()
        );
    }

    println!("\n== memory map after load ==");
    for r in &report.memory_map.regions {
        println!(
            "  {:>8}  {:#012x} .. {:#012x}  ({} bytes)",
            r.kind.label(),
            r.base,
            r.base + r.size,
            r.size
        );
    }

    println!("\n== hand off ==");
    println!("  cpu mode   = {:?}", report.machine.cpu.mode);
    println!("  a20        = {}", report.machine.cpu.a20);
    println!("  entry (ip) = {:#x}", report.machine.cpu.ip);
    println!(
        "  kernel     = '{}' saw {} memory map region(s)",
        report.outcome.marker, report.outcome.regions_seen
    );
    println!("\nboot complete.");
}

fn print_help() {
    println!("ignition - a deterministic boot sequence simulator");
    println!();
    println!("usage:");
    println!("  ignition demo           boot the bundled demo disk image");
    println!("  ignition boot [image]   boot a disk image file (or the demo if omitted)");
    println!("  ignition help           show this help");
    println!();
    println!("more at https://github.com/pavanchow/ignition");
}
