//! Local review bundles only. Never tags, commits, pushes or publishes.
use crate::{KernelVariant, Options, build_image, check_private, workspace_root};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const VERSION: &str = "0.1.0-experimental.1";

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to.parent().ok_or("missing parent")?).map_err(|e| e.to_string())?;
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", from.display()))
}
fn output(command: &mut Command) -> Result<String, String> {
    // All identity/toolchain queries must describe the packaged workspace,
    // including when the compiled xtask is invoked from another repository.
    command.current_dir(workspace_root());
    let out = command.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A deliberately narrow reader for Cargo-generated lockfile string fields.
fn field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    block
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key} = \""))?.strip_suffix('"'))
}

fn dependencies(dir: &Path) -> Result<(), String> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|p| PathBuf::from(p).join(".cargo"))
        })
        .ok_or("CARGO_HOME/home unavailable")?;
    let registry = home.join("registry");
    let sources = fs::read_dir(registry.join("src"))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect::<Vec<_>>();
    let find = |name: &str| {
        sources
            .iter()
            .map(|p| p.join(name))
            .find(|p| p.is_dir())
            .ok_or(format!("missing cached source {name}; build image first"))
    };
    let boot = find("bootloader-x86_64-uefi-0.11.17")?;
    // Include optional/other-target dependencies too, so a first release
    // does not depend on an accidentally fully populated developer cache.
    let mut fetch = crate::cargo();
    fetch.args(["fetch", "--locked"]);
    crate::status(fetch)?;
    let mut fetch = crate::cargo();
    fetch
        .args(["fetch", "--locked", "--manifest-path"])
        .arg(boot.join("Cargo.toml"));
    crate::status(fetch)?;
    let locks = [workspace_root().join("Cargo.lock"), boot.join("Cargo.lock")];
    let mut packages = BTreeMap::new();
    for (index, path) in locks.iter().enumerate() {
        copy(path, &dir.join(format!("dependencies/lock-{index}.toml")))?;
        let lock = String::from_utf8(read(path)?).map_err(|e| e.to_string())?;
        for block in lock.split("[[package]]").skip(1) {
            let name = field(block, "name").ok_or("lockfile package without name")?;
            let version = field(block, "version").ok_or("lockfile package without version")?;
            // Workspace packages have no registry checksum. The UEFI root
            // and its path siblings are nevertheless published crate sources.
            if let Some(hash) = field(block, "checksum") {
                packages.insert(format!("{name}-{version}"), Some(hash.to_string()));
            } else if name.starts_with("bootloader") {
                packages.entry(format!("{name}-{version}")).or_insert(None);
            }
        }
    }
    let mut audit = String::from(
        "# Exact dependency source inventory\n\nSuperset of workspace and UEFI lockfiles (includes host/build/optional dependencies).\nFull unmodified crate archives preserve source and notices; ucs2 source is included for MPL-2.0.\n\n| Crate | Declared license | Archive SHA-256 |\n| --- | --- | --- |\n",
    );
    for (name, expected) in packages {
        let source = find(&name)?;
        let registry_name = source
            .parent()
            .and_then(Path::file_name)
            .ok_or("registry path")?;
        let archive = registry
            .join("cache")
            .join(registry_name)
            .join(format!("{name}.crate"));
        let hash = digest(&read(&archive)?);
        if expected.as_ref().is_some_and(|v| *v != hash) {
            return Err(format!("checksum mismatch: {name}"));
        }
        let manifest =
            String::from_utf8(read(&source.join("Cargo.toml"))?).map_err(|e| e.to_string())?;
        let license =
            field(&manifest, "license").ok_or(format!("missing declared license: {name}"))?;
        audit.push_str(&format!("| {name} | {license} | {hash} |\n"));
        copy(
            &archive,
            &dir.join("dependencies/sources")
                .join(format!("{name}.crate")),
        )?;
        for entry in fs::read_dir(&source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let filename = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_file()
                && ["LICENSE", "COPYING", "NOTICE"]
                    .iter()
                    .any(|p| filename.to_uppercase().starts_with(p))
            {
                copy(
                    &entry.path(),
                    &dir.join("dependencies/notices").join(&name).join(filename),
                )?;
            }
        }
    }
    let sysroot = output(Command::new("rustc").args(["--print", "sysroot"]))?;
    let toolchain = Path::new(sysroot.trim()).join("share/doc/rust");
    // Rust distribution copyright includes compiler_builtins/LLVM exceptions.
    for file in [
        "COPYRIGHT.html",
        "COPYRIGHT-library.html",
        "licenses/MIT.txt",
        "licenses/Apache-2.0.txt",
        "licenses/LLVM-exception.txt",
    ] {
        copy(
            &toolchain.join(file),
            &dir.join("dependencies/rust").join(file),
        )?;
    }
    fs::write(dir.join("dependencies/INVENTORY.md"), audit).map_err(|e| e.to_string())
}

fn public_path(path: &str) -> bool {
    let p = Path::new(path);
    !p.is_absolute()
        && !path
            .split('/')
            .any(|s| matches!(s, ".." | ".agent-private" | ".git"))
        && ([
            "crates/",
            "kernel/",
            "userspace/",
            "xtask/",
            "tools/",
            "docs/",
            "licenses/",
            ".github/",
            ".cargo/",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
            || [
                "AGENTS.md",
                "CLAUDE.md",
                "README.md",
                "CONTRIBUTING.md",
                "SECURITY.md",
                "LICENSE",
                "THIRD_PARTY_NOTICES.md",
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
                "rustfmt.toml",
                ".gitignore",
            ]
            .contains(&path))
        && !crate::is_private(path)
}

fn checksums(dir: &Path, at: &Path, lines: &mut Vec<String>) -> Result<(), String> {
    for entry in fs::read_dir(at).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            checksums(dir, &path, lines)?;
        } else {
            lines.push(format!(
                "{}  {}",
                digest(&read(&path)?),
                path.strip_prefix(dir)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/")
            ));
        }
    }
    Ok(())
}

pub fn run(options: &Options) -> Result<(), String> {
    if !options.release {
        return Err("release packaging requires --release".into());
    }
    check_private()?;
    let image = build_image(options, KernelVariant::FaultTest("shell"))?;
    let dir = workspace_root().join("target/releases").join(VERSION);
    // Refuse overwrite, including partial candidates: reviewer evidence survives.
    fs::create_dir_all(dir.parent().ok_or("release parent")?).map_err(|e| e.to_string())?;
    fs::create_dir(&dir)
        .map_err(|e| format!("fresh release directory required {}: {e}", dir.display()))?;
    let result = (|| {
        let paths = output(Command::new("git").current_dir(workspace_root()).args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]))?;
        for path in paths.split('\0').filter(|p| !p.is_empty()) {
            if !public_path(path) {
                return Err(format!("unapproved release source path: {path}"));
            }
            let source = workspace_root().join(path);
            if fs::symlink_metadata(&source)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err(format!("source symlink refused: {path}"));
            }
            copy(&source, &dir.join("source").join(path))?;
        }
        dependencies(&dir)?;
        copy(&image, &dir.join(format!("rust-os-{VERSION}-qemu.img")))?;
        copy(&workspace_root().join("LICENSE"), &dir.join("LICENSE"))?;
        copy(
            &workspace_root().join("THIRD_PARTY_NOTICES.md"),
            &dir.join("THIRD_PARTY_NOTICES.md"),
        )?;
        for name in ["Noto-OFL.txt", "noto-sans-mono-bitmap-MIT.txt"] {
            copy(
                &workspace_root().join("licenses").join(name),
                &dir.join("licenses").join(name),
            )?;
        }
        copy(
            &workspace_root().join("docs/release.md"),
            &dir.join("RELEASE-NOTES.md"),
        )?;
        let revision = output(Command::new("git").args(["rev-parse", "HEAD"]))?;
        let state = output(Command::new("git").args(["status", "--porcelain"]))?;
        let rust = output(Command::new("rustc").arg("-Vv"))?;
        fs::write(dir.join("BUILD.txt"), format!("Candidate {VERSION}\nBase revision: {}\nSource includes working tree changes: {}\n{rust}\nCommand: cargo xtask release --release\nNot published. Checksums establish integrity, not a signature. No bit-identical rebuild claim.\n", revision.trim(), !state.is_empty())).map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        checksums(&dir, &dir, &mut lines)?;
        lines.sort();
        fs::write(dir.join("SHA256SUMS"), lines.join("\n") + "\n").map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = result {
        fs::write(dir.join("INCOMPLETE.txt"), &error).map_err(|e| e.to_string())?;
        return Err(error);
    }
    eprintln!(
        "Review candidate: {}. Acceptance evidence must be reviewed separately; no publication performed.",
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_excludes_private_and_unknown_paths() {
        for p in [
            ".agent-private/STATE.md",
            "docs/prompts/request.md",
            "docs/PROMPTS.md",
            "docs/Prompts/request.md",
            "docs/PROJECT/STATE.md",
            "../secret",
            ".env",
            ".git/config",
            "MEMORY.md",
        ] {
            assert!(!public_path(p));
        }
        for p in ["docs/release.md", "kernel/src/main.rs", "Cargo.lock"] {
            assert!(public_path(p));
        }
    }
    #[test]
    fn checksum_matches_standard_vector() {
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
