//! Packaging only: converted images do not establish hypervisor compatibility.
use crate::{KernelVariant, Options, build_image, qemu, status, workspace_root};
use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn run(options: &Options) -> Result<(), String> {
    let source = build_image(options, KernelVariant::FaultTest("shell"))?;
    let qemu = qemu::program()?;
    let converter = qemu.with_file_name(if cfg!(windows) {
        "qemu-img.exe"
    } else {
        "qemu-img"
    });
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir = workspace_root()
        .join("target/vm-images")
        .join(format!("{}-{stamp}", options.profile_dir()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (format, name) in [("vdi", "rust-os.vdi"), ("vmdk", "rust-os.vmdk")] {
        let mut cmd = Command::new(&converter);
        cmd.args(["convert", "-f", "raw", "-O", format])
            .arg(&source)
            .arg(dir.join(name));
        status(cmd)?;
        let mut check = Command::new(&converter);
        check.arg("info").arg(dir.join(name));
        status(check)?;
        let mut compare = Command::new(&converter);
        compare
            .args(["compare", "-f", "raw", "-F", format])
            .arg(&source)
            .arg(dir.join(name));
        status(compare)?;
    }
    fs::copy(&source, dir.join("rust-os-qemu.img")).map_err(|e| e.to_string())?;
    fs::write(dir.join("rust-os.vmx"), "config.version = \"8\"\nvirtualHW.version = \"14\"\ndisplayName = \"rust-os experimental (unverified)\"\nguestOS = \"other-64\"\nfirmware = \"efi\"\nmemsize = \"256\"\nnumvcpus = \"1\"\nide0:0.present = \"TRUE\"\nide0:0.fileName = \"rust-os.vmdk\"\nethernet0.present = \"FALSE\"\nusb.present = \"FALSE\"\n").map_err(|e| e.to_string())?;
    eprintln!(
        "VM images: {} (VirtualBox/VMware boot and power UNVERIFIED; see docs/power.md)",
        dir.display()
    );
    Ok(())
}
