//! Project build tool. Run `cargo xtask help`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const IMAGE_NAME: &str = "rust-os-uefi.img";

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
  lint            rustfmt --check, clippy (host crates and kernel target), deny warnings
  test            host unit tests for the default workspace members
  build           build the kernel ELF for x86_64-unknown-none
  image           build the kernel and a UEFI GPT disk image (network on first run)
  run             build the image and launch it in QEMU (NOT yet verified)
  check-private   fail if private paths are tracked or staged in Git
  ci              lint + test + build + check-private
  help            show this message

options:
  --release                 build the kernel with the release profile
  --ovmf-code <path>        OVMF code firmware for `run` (or RUST_OS_OVMF_CODE)
  --ovmf-vars <path>        OVMF variable store template for `run` (or RUST_OS_OVMF_VARS)
  --timeout <secs>          stop QEMU after this many seconds (default: none)
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
        "build" => build_kernel(&options).map(drop),
        "image" => build_image(&options).map(drop),
        "run" => run(&options),
        "check-private" => check_private(),
        "ci" => lint()
            .and_then(|()| test())
            .and_then(|()| build_kernel(&options).map(drop))
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
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options::default();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
            match arg.as_str() {
                "--release" => options.release = true,
                "--ovmf-code" => options.ovmf_code = Some(value("--ovmf-code")?.into()),
                "--ovmf-vars" => options.ovmf_vars = Some(value("--ovmf-vars")?.into()),
                "--timeout" => {
                    let secs = value("--timeout")?;
                    let secs: u64 = secs
                        .parse()
                        .map_err(|_| format!("invalid --timeout `{secs}`"))?;
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

    let mut kernel = cargo();
    kernel.args([
        "clippy",
        "--package",
        "kernel",
        "--target",
        KERNEL_TARGET,
        "--",
        "-D",
        "warnings",
    ]);
    status(kernel)
}

fn test() -> Result<(), String> {
    let mut cmd = cargo();
    cmd.arg("test");
    status(cmd)
}

fn build_kernel(options: &Options) -> Result<PathBuf, String> {
    let mut cmd = cargo();
    cmd.args(["build", "--package", "kernel", "--target", KERNEL_TARGET]);
    if options.release {
        cmd.arg("--release");
    }
    status(cmd)?;
    let elf = workspace_root()
        .join("target")
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

fn build_image(options: &Options) -> Result<PathBuf, String> {
    let kernel = build_kernel(options)?;
    let image = workspace_root()
        .join("target")
        .join("images")
        .join(options.profile_dir())
        .join(IMAGE_NAME);
    let mut cmd = cargo();
    cmd.args(["run", "--release", "--package", "image-builder", "--"]);
    cmd.arg(&kernel).arg(&image);
    status(cmd)?;
    Ok(image)
}

fn run(options: &Options) -> Result<(), String> {
    let ovmf_code = options.ovmf_code.as_ref().ok_or(
        "QEMU needs UEFI firmware: pass --ovmf-code <OVMF_CODE.fd> or set RUST_OS_OVMF_CODE",
    )?;
    if !ovmf_code.is_file() {
        return Err(format!(
            "OVMF code firmware not found: {}",
            ovmf_code.display()
        ));
    }
    let image = build_image(options)?;

    let mut drives: Vec<OsString> = vec![pflash(ovmf_code, true)];
    if let Some(vars_template) = &options.ovmf_vars {
        // Copy the variable store so the guest never writes the template.
        let vars = image.with_file_name("OVMF_VARS.fd");
        std::fs::copy(vars_template, &vars)
            .map_err(|err| format!("cannot copy OVMF vars {}: {err}", vars_template.display()))?;
        drives.push(pflash(&vars, false));
    }
    let mut disk = OsString::from("format=raw,file=");
    disk.push(&image);

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-machine", "q35", "-m", "256M", "-serial", "stdio"]);
    cmd.args(["-no-reboot", "-no-shutdown"]);
    for drive in drives {
        cmd.arg("-drive").arg(drive);
    }
    cmd.arg("-drive").arg(disk);
    eprintln!("$ {cmd:?}");
    let mut child = cmd.spawn().map_err(|err| {
        format!("failed to start qemu-system-x86_64 (is QEMU installed and on PATH?): {err}")
    })?;

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

fn pflash(path: &Path, readonly: bool) -> OsString {
    let mut arg = OsString::from("if=pflash,format=raw,");
    if readonly {
        arg.push("readonly=on,");
    }
    arg.push("file=");
    arg.push(path);
    arg
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
            ["--release", "--timeout", "5"]
                .map(String::from)
                .into_iter(),
        )
        .expect("valid options");
        assert!(opts.release);
        assert_eq!(opts.timeout, Some(Duration::from_secs(5)));
        assert!(Options::parse(["--bogus".to_string()].into_iter()).is_err());
        assert!(Options::parse(["--timeout".to_string()].into_iter()).is_err());
    }
}
