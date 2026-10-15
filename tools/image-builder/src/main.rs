//! `image-builder <kernel-elf> <output-image>`
//!
//! Kept separate from xtask because the `bootloader` dependency compiles the
//! UEFI bootloader stage (nightly, `-Zbuild-std`, network on first build).
//! Ordinary `cargo test` / `cargo xtask lint` therefore stay fast and offline.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(kernel), Some(out), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: image-builder <kernel-elf> <output-image>");
        return ExitCode::from(2);
    };
    let (kernel, out) = (PathBuf::from(kernel), PathBuf::from(out));

    if !kernel.is_file() {
        eprintln!("kernel ELF not found: {}", kernel.display());
        return ExitCode::FAILURE;
    }
    if let Some(parent) = out.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!("cannot create {}: {err}", parent.display());
        return ExitCode::FAILURE;
    }

    match bootloader::UefiBoot::new(&kernel).create_disk_image(&out) {
        Ok(()) => {
            println!("UEFI disk image: {}", out.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("failed to create UEFI disk image: {err:#}");
            ExitCode::FAILURE
        }
    }
}
