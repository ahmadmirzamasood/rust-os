//! Building the user programs and the boot program bundle.
//!
//! The programs in `userspace/apps` are built for `x86_64-unknown-none` as
//! static, non-position-independent executables linked at
//! [`IMAGE_BASE`] (inside the user image range of `os_abi::layout`), into
//! their own target directory so these flags never touch the kernel build.
//! Each ELF is then checked with the same parser the kernel's loader uses,
//! and all of them are packed into one bundle that the image builder hands
//! to the bootloader as the ramdisk.

use std::path::PathBuf;

use exec::elf::{self, Limits};
use os_abi::layout;

use crate::{KERNEL_TARGET, Options, cargo, status, workspace_root};

/// Programs packed into the bundle, in this order. `init` is started by
/// the kernel; the others are started by `init`.
pub const PROGRAMS: [&str; 5] = ["init", "echo", "spin", "fault", "abuse"];

/// Link address of every user program.
const IMAGE_BASE: u64 = 0x40_0000;

const LIMITS: Limits = Limits {
    start: layout::IMAGE_START,
    end: layout::IMAGE_END,
    max_bytes: layout::IMAGE_MAX_BYTES,
};

fn target_dir() -> PathBuf {
    workspace_root().join("target").join("user")
}

/// `CARGO_ENCODED_RUSTFLAGS` for user programs: a static executable at a
/// fixed address, without debug information. (Encoded flags take precedence over `RUSTFLAGS` and any
/// config, so the user build is the same everywhere.)
fn rustflags() -> String {
    [
        "-Crelocation-model=static".to_owned(),
        format!("-Clink-arg=--image-base={IMAGE_BASE:#x}"),
        // Nothing in the guest reads DWARF; keeps the bundle small.
        "-Cstrip=debuginfo".to_owned(),
    ]
    .join("\u{1f}")
}

/// Build the programs, validate them and write the bundle. Returns its path.
pub fn build_bundle(options: &Options) -> Result<PathBuf, String> {
    const _: () = assert!(IMAGE_BASE >= layout::IMAGE_START && IMAGE_BASE < layout::IMAGE_END);
    let mut cmd = cargo();
    cmd.args(["build", "--package", "apps", "--target", KERNEL_TARGET]);
    cmd.arg("--target-dir").arg(target_dir());
    if options.release {
        cmd.arg("--release");
    }
    cmd.env("CARGO_ENCODED_RUSTFLAGS", rustflags());
    cmd.env_remove("RUSTFLAGS");
    status(cmd)?;

    let bin_dir = target_dir().join(KERNEL_TARGET).join(options.profile_dir());
    let mut files = Vec::new();
    for name in PROGRAMS {
        let path = bin_dir.join(name);
        let data =
            std::fs::read(&path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        let image = elf::parse(&data, &LIMITS).map_err(|err| {
            format!(
                "{} is not a valid user executable: {}",
                path.display(),
                err.description()
            )
        })?;
        eprintln!(
            "user program {name}: {} KiB, {} segments, entry {:#x}",
            data.len() / 1024,
            image.segments().len(),
            image.entry
        );
        files.push((name, data));
    }
    let entries: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    let bundle = exec::bundle::build(&entries).map_err(|err| format!("bundle: {err:?}"))?;
    let out = bin_dir.join("programs.bundle");
    std::fs::write(&out, &bundle)
        .map_err(|err| format!("cannot write {}: {err}", out.display()))?;
    eprintln!(
        "program bundle: {} ({} KiB, {} programs)",
        out.display(),
        bundle.len() / 1024,
        PROGRAMS.len()
    );
    Ok(out)
}

/// Clippy for the user crates (bare-metal target).
pub fn lint() -> Result<(), String> {
    let mut cmd = cargo();
    cmd.args([
        "clippy",
        "--package",
        "ulib",
        "--package",
        "apps",
        "--target",
        KERNEL_TARGET,
    ]);
    cmd.arg("--target-dir").arg(target_dir());
    cmd.args(["--", "-D", "warnings"]);
    status(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustflags_are_unit_separated() {
        let flags = rustflags();
        let parts: Vec<&str> = flags.split('\u{1f}').collect();
        assert_eq!(
            parts,
            [
                "-Crelocation-model=static",
                "-Clink-arg=--image-base=0x400000",
                "-Cstrip=debuginfo"
            ]
        );
    }

    #[test]
    fn program_names_are_valid_bundle_names() {
        for name in PROGRAMS {
            assert!(exec::bundle::valid_name(name), "{name}");
        }
        assert!(PROGRAMS.contains(&"init"), "the kernel starts `init`");
    }
}
