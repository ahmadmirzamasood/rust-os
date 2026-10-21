//! Locating QEMU and UEFI firmware, and the common QEMU command line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const QEMU_NAME: &str = "qemu-system-x86_64";

/// Firmware locations tried when none is given, as (code, vars template).
/// The vars template is optional.
const FIRMWARE_CANDIDATES: &[(&str, &str)] = &[
    // QEMU for Windows (winget SoftwareFreedomConservancy.QEMU).
    (
        r"C:\Program Files\qemu\share\edk2-x86_64-code.fd",
        r"C:\Program Files\qemu\share\edk2-i386-vars.fd",
    ),
    // Debian / Ubuntu `ovmf`.
    (
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ),
    (
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_VARS.fd",
    ),
    // Fedora `edk2-ovmf`.
    (
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    ),
    // Arch `edk2-ovmf`.
    (
        "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
        "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
    ),
    // Firmware bundled with upstream QEMU installs (Linux, Homebrew).
    (
        "/usr/share/qemu/edk2-x86_64-code.fd",
        "/usr/share/qemu/edk2-i386-vars.fd",
    ),
    (
        "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
        "/opt/homebrew/share/qemu/edk2-i386-vars.fd",
    ),
    (
        "/usr/local/share/qemu/edk2-x86_64-code.fd",
        "/usr/local/share/qemu/edk2-i386-vars.fd",
    ),
];

/// UEFI firmware for QEMU.
#[derive(Debug, Clone)]
pub struct Firmware {
    pub code: PathBuf,
    /// Variable-store template; copied before use, never written.
    pub vars: Option<PathBuf>,
}

/// Use the given firmware files, or search the usual install locations.
pub fn firmware(code: Option<&Path>, vars: Option<&Path>) -> Result<Firmware, String> {
    if let Some(code) = code {
        if !code.is_file() {
            return Err(format!("OVMF code firmware not found: {}", code.display()));
        }
        if let Some(vars) = vars
            && !vars.is_file()
        {
            return Err(format!("OVMF vars template not found: {}", vars.display()));
        }
        return Ok(Firmware {
            code: code.to_path_buf(),
            vars: vars.map(Path::to_path_buf),
        });
    }
    for (code, vars) in FIRMWARE_CANDIDATES {
        let code = Path::new(code);
        if code.is_file() {
            let vars = Path::new(vars);
            return Ok(Firmware {
                code: code.to_path_buf(),
                vars: vars.is_file().then(|| vars.to_path_buf()),
            });
        }
    }
    Err(
        "no UEFI firmware found: pass --ovmf-code <OVMF_CODE.fd> (and --ovmf-vars) \
         or set RUST_OS_OVMF_CODE / RUST_OS_OVMF_VARS; see docs/building.md"
            .into(),
    )
}

/// `RUST_OS_QEMU`, else `qemu-system-x86_64` from `PATH`, else the default
/// Windows install location.
pub fn program() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RUST_OS_QEMU") {
        return Ok(PathBuf::from(path));
    }
    let exe = format!("{QEMU_NAME}{}", std::env::consts::EXE_SUFFIX);
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let windows_default = Path::new(r"C:\Program Files\qemu").join(&exe);
    if cfg!(windows) && windows_default.is_file() {
        return Ok(windows_default);
    }
    Err(format!(
        "{QEMU_NAME} not found: install QEMU and put it on PATH, or set RUST_OS_QEMU"
    ))
}

/// The machine every run uses: q35, one CPU, 256 MiB, no reboot on triple
/// fault, firmware in pflash, the image as a raw disk. `vars_copy` must be a
/// writable copy of the firmware vars template (if any).
pub fn base_command(
    qemu: &Path,
    firmware: &Firmware,
    vars_copy: Option<&Path>,
    image: &Path,
) -> Command {
    let mut cmd = Command::new(qemu);
    cmd.args(["-machine", "q35", "-m", "256M", "-smp", "1"]);
    cmd.args(["-no-reboot", "-no-shutdown"]);
    cmd.arg("-drive").arg(pflash(&firmware.code, true));
    if let Some(vars) = vars_copy {
        cmd.arg("-drive").arg(pflash(vars, false));
    }
    let mut disk = OsString::from("format=raw,snapshot=on,file=");
    disk.push(image);
    cmd.arg("-drive").arg(disk);
    cmd
}

/// Interactive and power tests require real guest reset and power-off.
/// Diagnostic smoke deliberately keeps its no-reboot/no-shutdown guards.
pub fn enable_power(cmd: Command) -> Command {
    let mut power = Command::new(cmd.get_program());
    power.args(
        cmd.get_args()
            .filter(|a| *a != "-no-reboot" && *a != "-no-shutdown"),
    );
    power
}

/// Copy the vars template next to `dir` so the guest never modifies it.
pub fn copy_vars(firmware: &Firmware, dir: &Path) -> Result<Option<PathBuf>, String> {
    let Some(template) = &firmware.vars else {
        return Ok(None);
    };
    let copy = dir.join("OVMF_VARS.fd");
    std::fs::copy(template, &copy)
        .map_err(|err| format!("cannot copy OVMF vars {}: {err}", template.display()))?;
    Ok(Some(copy))
}

fn pflash(path: &Path, readonly: bool) -> OsString {
    let mut arg = OsString::from("if=pflash,format=raw,");
    if readonly {
        arg.push("readonly=on,");
    }
    arg.push("file=");
    arg.push(path);
    arg
}
