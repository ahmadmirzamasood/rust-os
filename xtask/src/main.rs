//! Project build tool. Run `cargo xtask help`.

mod picture;
mod qemu;
mod smoke;
mod user;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use smoke::Scenario;

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const IMAGE_NAME: &str = "rust-os-uefi.img";
/// Kernel features that turn the kernel into a fault-test build. Each is
/// built into its own target directory and image, never the normal ones.
const FAULT_TEST_FEATURES: [&str; 4] = [
    "deliberate-panic",
    "deliberate-page-fault",
    "deliberate-stack-overflow",
    "deliberate-interrupt-fault",
];
/// Default per-scenario limit for `smoke`, measured from QEMU start.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(120);

/// Path prefixes that must never be tracked or staged in the public repository.
const PRIVATE_PREFIXES: &[&str] = &[
    ".agent-private/",
    "GETTING_STARTED_PRIVATE.md",
    "docs/prompts/",
    "docs/phases/",
    "docs/project/",
    "docs/subsystems/",
    "queries/",
    "prompts/",
    "private/",
    ".claude/",
    ".codex/",
];

const HELP: &str = "\
usage: cargo xtask <command> [options]

commands:
  lint            rustfmt --check, clippy (host crates, kernel and user programs on
                  the bare-metal target), deny warnings
  test            host unit tests for the default workspace members
  build           build the kernel ELF for x86_64-unknown-none
  user            build the user programs, validate them, write the program bundle
  image           build kernel, user programs and a UEFI GPT disk image whose
                  ramdisk is the program bundle (network on first run)
  run             build the image and boot it in QEMU (window + serial on stdio)
  smoke           boot smoke test in headless QEMU: serial markers + screenshot check
  check-private   fail if private paths are tracked or staged in Git
  ci              lint + test + build + user + check-private (no QEMU needed)
  help            show this message

options:
  --release                 build the kernel with the release profile
  --ovmf-code <path>        OVMF code firmware (or RUST_OS_OVMF_CODE; else auto-detected)
  --ovmf-vars <path>        OVMF variable store template (or RUST_OS_OVMF_VARS)
  --timeout <secs>          run: stop QEMU after this long (default: none)
                            smoke: per-scenario limit (default: 120)
  --headless                run: no display window
  --scenario <name>         smoke: boot, panic, no-display, page-fault,
                            stack-overflow, interrupt-fault, input, processes
                            or all (default: all)

QEMU is `qemu-system-x86_64` from PATH, or RUST_OS_QEMU.
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprint!("{HELP}");
        return ExitCode::from(2);
    };
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    let result = match command.as_str() {
        "lint" => lint(),
        "test" => test(),
        "build" => build_kernel(&options, KernelVariant::Normal).map(drop),
        "user" => user::build_bundle(&options).map(drop),
        "image" => build_image(&options, KernelVariant::Normal).map(drop),
        "run" => run(&options),
        "smoke" => smoke(&options),
        "check-private" => check_private(),
        "ci" => lint()
            .and_then(|()| test())
            .and_then(|()| build_kernel(&options, KernelVariant::Normal).map(drop))
            .and_then(|()| user::build_bundle(&options).map(drop))
            .and_then(|()| check_private()),
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`; see `cargo xtask help`")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Default)]
struct Options {
    release: bool,
    ovmf_code: Option<PathBuf>,
    ovmf_vars: Option<PathBuf>,
    timeout: Option<Duration>,
    headless: bool,
    scenario: Option<String>,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options::default();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
            match arg.as_str() {
                "--release" => options.release = true,
                "--headless" => options.headless = true,
                "--ovmf-code" => options.ovmf_code = Some(value("--ovmf-code")?.into()),
                "--ovmf-vars" => options.ovmf_vars = Some(value("--ovmf-vars")?.into()),
                "--scenario" => options.scenario = Some(value("--scenario")?),
                "--timeout" => {
                    let secs = value("--timeout")?;
                    let secs: u64 = secs
                        .parse()
                        .ok()
                        .filter(|&s| s > 0)
                        .ok_or(format!("invalid --timeout `{secs}`"))?;
                    options.timeout = Some(Duration::from_secs(secs));
                }
                other => return Err(format!("unknown option `{other}`")),
            }
        }
        options.ovmf_code = options
            .ovmf_code
            .or_else(|| std::env::var_os("RUST_OS_OVMF_CODE").map(PathBuf::from));
        options.ovmf_vars = options
            .ovmf_vars
            .or_else(|| std::env::var_os("RUST_OS_OVMF_VARS").map(PathBuf::from));
        Ok(options)
    }

    fn profile_dir(&self) -> &'static str {
        if self.release { "release" } else { "debug" }
    }

    fn firmware(&self) -> Result<qemu::Firmware, String> {
        let firmware = qemu::firmware(self.ovmf_code.as_deref(), self.ovmf_vars.as_deref())?;
        let vars = firmware
            .vars
            .as_deref()
            .map_or("none".into(), |v| v.display().to_string());
        eprintln!(
            "firmware: {} (vars template: {vars})",
            firmware.code.display()
        );
        Ok(firmware)
    }
}

/// Which kernel build to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KernelVariant {
    Normal,
    /// Built with one of [`FAULT_TEST_FEATURES`] into its own target
    /// directory (`target/fault-tests/<feature>`) and image, so it never
    /// replaces the normal kernel or image.
    FaultTest(&'static str),
}

impl KernelVariant {
    fn target_dir(self) -> PathBuf {
        match self {
            Self::Normal => workspace_root().join("target"),
            Self::FaultTest(feature) => workspace_root()
                .join("target")
                .join("fault-tests")
                .join(feature),
        }
    }

    fn image_name(self) -> String {
        match self {
            Self::Normal => IMAGE_NAME.to_owned(),
            Self::FaultTest(feature) => format!("rust-os-uefi-{feature}.img"),
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn cargo() -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(workspace_root());
    cmd
}

fn status(mut cmd: Command) -> Result<(), String> {
    let shown = format!("{cmd:?}");
    eprintln!("$ {shown}");
    let status = cmd
        .status()
        .map_err(|err| format!("failed to start {shown}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed ({status}): {shown}"))
    }
}

fn lint() -> Result<(), String> {
    let mut fmt = cargo();
    fmt.args(["fmt", "--all", "--check"]);
    status(fmt)?;

    let mut host = cargo();
    host.args(["clippy", "--all-targets", "--", "-D", "warnings"]);
    status(host)?;

    let variants = std::iter::once(None).chain(FAULT_TEST_FEATURES.map(Some));
    for features in variants {
        let mut kernel = cargo();
        kernel.args(["clippy", "--package", "kernel", "--target", KERNEL_TARGET]);
        if let Some(features) = features {
            kernel.args(["--features", features]);
        }
        kernel.args(["--", "-D", "warnings"]);
        status(kernel)?;
    }
    user::lint()
}

fn test() -> Result<(), String> {
    let mut cmd = cargo();
    cmd.arg("test");
    status(cmd)
}

fn build_kernel(options: &Options, variant: KernelVariant) -> Result<PathBuf, String> {
    let target_dir = variant.target_dir();
    let mut cmd = cargo();
    cmd.args(["build", "--package", "kernel", "--target", KERNEL_TARGET]);
    if let KernelVariant::FaultTest(feature) = variant {
        cmd.args(["--features", feature]);
        cmd.arg("--target-dir").arg(&target_dir);
    }
    if options.release {
        cmd.arg("--release");
    }
    status(cmd)?;
    let elf = target_dir
        .join(KERNEL_TARGET)
        .join(options.profile_dir())
        .join("kernel");
    if elf.is_file() {
        eprintln!("kernel ELF: {}", elf.display());
        Ok(elf)
    } else {
        Err(format!("expected kernel ELF at {}", elf.display()))
    }
}

fn build_image(options: &Options, variant: KernelVariant) -> Result<PathBuf, String> {
    let kernel = build_kernel(options, variant)?;
    let bundle = user::build_bundle(options)?;
    let image = workspace_root()
        .join("target")
        .join("images")
        .join(options.profile_dir())
        .join(variant.image_name());
    let mut cmd = cargo();
    cmd.args(["run", "--release", "--package", "image-builder", "--"]);
    cmd.arg(&kernel).arg(&image).arg(&bundle);
    status(cmd)?;
    Ok(image)
}

fn run(options: &Options) -> Result<(), String> {
    let firmware = options.firmware()?;
    let qemu = qemu::program()?;
    let image = build_image(options, KernelVariant::Normal)?;
    let image_dir = image.parent().ok_or("image has no parent directory")?;
    let vars = qemu::copy_vars(&firmware, image_dir)?;

    let mut cmd = qemu::base_command(&qemu, &firmware, vars.as_deref(), &image);
    cmd.args(["-serial", "stdio"]);
    if options.headless {
        cmd.args(["-display", "none"]);
    }
    eprintln!("$ {cmd:?}");
    let mut child = cmd
        .spawn()
        .map_err(|err| format!("failed to start {}: {err}", qemu.display()))?;

    let started = Instant::now();
    loop {
        if let Some(exit) = child.try_wait().map_err(|err| err.to_string())? {
            return if exit.success() {
                Ok(())
            } else {
                Err(format!("QEMU exited with {exit}"))
            };
        }
        if options
            .timeout
            .is_some_and(|limit| started.elapsed() >= limit)
        {
            child.kill().map_err(|err| err.to_string())?;
            let _ = child.wait();
            eprintln!("QEMU stopped after timeout");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn smoke(options: &Options) -> Result<(), String> {
    let scenarios = Scenario::parse(options.scenario.as_deref().unwrap_or("all"))?;
    let setup = smoke::Setup {
        qemu: qemu::program()?,
        firmware: options.firmware()?,
        timeout: options.timeout.unwrap_or(SMOKE_TIMEOUT),
        out_root: workspace_root()
            .join("target")
            .join("smoke")
            .join(options.profile_dir()),
    };
    eprintln!("qemu: {}", setup.qemu.display());

    // Build every needed image first so build time never counts against the
    // boot timeout.
    let variant = |scenario: Scenario| match scenario.kernel_feature() {
        Some(feature) => KernelVariant::FaultTest(feature),
        None => KernelVariant::Normal,
    };
    let mut images = std::collections::HashMap::new();
    for &scenario in &scenarios {
        let variant = variant(scenario);
        if let std::collections::hash_map::Entry::Vacant(slot) = images.entry(variant) {
            slot.insert(build_image(options, variant)?);
        }
    }

    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for scenario in scenarios {
        let image = images
            .get(&variant(scenario))
            .ok_or("image was not built")?;
        match smoke::run(scenario, image, &setup) {
            Ok(summary) => passed.push(summary),
            Err(err) => {
                eprintln!("{err}");
                failed.push(scenario.name());
            }
        }
    }
    eprintln!("--- smoke summary ---");
    for summary in &passed {
        eprintln!("{summary}");
    }
    if failed.is_empty() {
        eprintln!("smoke: all {} scenario(s) passed", passed.len());
        Ok(())
    } else {
        Err(format!("smoke scenario(s) failed: {}", failed.join(", ")))
    }
}

fn git_lines(args: &[&str]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn is_private(path: &str) -> bool {
    let path = path.replace('\\', "/");
    PRIVATE_PREFIXES.iter().any(|prefix| {
        let prefix = prefix.to_ascii_lowercase();
        path.to_ascii_lowercase().starts_with(&prefix)
    })
}

fn check_private() -> Result<(), String> {
    let mut exposed = git_lines(&["ls-files", "--cached"])?;
    exposed.extend(git_lines(&["diff", "--cached", "--name-only"])?);
    exposed.retain(|path| is_private(path));
    exposed.sort();
    exposed.dedup();
    if !exposed.is_empty() {
        return Err(format!(
            "private paths are tracked or staged:\n  {}",
            exposed.join("\n  ")
        ));
    }

    // The ignore rules must cover the private tree even when it is empty.
    let ignored = Command::new("git")
        .args([
            "check-ignore",
            "--quiet",
            "--no-index",
            ".agent-private/STATE.md",
        ])
        .current_dir(workspace_root())
        .status()
        .map_err(|err| format!("failed to run git: {err}"))?;
    if !ignored.success() {
        return Err(".agent-private/ is not ignored by .gitignore".into());
    }
    eprintln!("check-private: no private paths tracked or staged; .agent-private/ is ignored");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_paths_are_detected() {
        assert!(is_private(".agent-private/STATE.md"));
        assert!(is_private(".agent-private\\specs\\BOOT.md"));
        assert!(is_private(".Agent-Private/x"));
        assert!(is_private("GETTING_STARTED_PRIVATE.md"));
        assert!(is_private("docs/prompts/00.md"));
    }

    #[test]
    fn public_paths_are_allowed() {
        assert!(!is_private("README.md"));
        assert!(!is_private("docs/architecture/overview.md"));
        assert!(!is_private("kernel/src/main.rs"));
        assert!(!is_private("AGENTS.md"));
    }

    #[test]
    fn options_parse_and_reject_unknown() {
        let opts = Options::parse(
            [
                "--release",
                "--headless",
                "--timeout",
                "5",
                "--scenario",
                "panic",
            ]
            .map(String::from)
            .into_iter(),
        )
        .expect("valid options");
        assert!(opts.release);
        assert!(opts.headless);
        assert_eq!(opts.timeout, Some(Duration::from_secs(5)));
        assert_eq!(opts.scenario.as_deref(), Some("panic"));
        assert!(Options::parse(["--bogus".to_string()].into_iter()).is_err());
        assert!(Options::parse(["--timeout".to_string()].into_iter()).is_err());
        assert!(Options::parse(["--timeout", "0"].map(String::from).into_iter()).is_err());
    }
}
