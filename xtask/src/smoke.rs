//! Automated boot smoke test.
//!
//! Each scenario boots an image in headless QEMU with the guest serial port
//! written to a file and QMP (QEMU's control protocol) on stdio. The test
//! watches the serial log for the kernel's markers until an explicit success
//! or failure signal or the timeout. Where the scenario draws, it then takes
//! a QMP `screendump` and checks the pixels against the colours the kernel
//! uses (which also catches red/blue swaps), and saves a PNG.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use boot_data::markers;
use gfx::{Rgb, screens::count_color, theme};

use crate::picture::{self, RgbImage};
use crate::qemu::{self, Firmware};

/// What a smoke run boots and expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Normal image: welcome screen and `BOOT OK`.
    Boot,
    /// Image built with `deliberate-panic`: full panic report and banner.
    Panic,
    /// Normal image on a VM without a display adapter: graphics fails,
    /// serial diagnostics continue, `BOOT DEGRADED`.
    NoDisplay,
}

impl Scenario {
    pub const ALL: [Scenario; 3] = [Self::Boot, Self::Panic, Self::NoDisplay];

    pub fn parse(name: &str) -> Result<Vec<Scenario>, String> {
        match name {
            "all" => Ok(Self::ALL.to_vec()),
            "boot" => Ok(vec![Self::Boot]),
            "panic" => Ok(vec![Self::Panic]),
            "no-display" => Ok(vec![Self::NoDisplay]),
            other => Err(format!(
                "unknown scenario `{other}` (expected boot, panic, no-display or all)"
            )),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::Panic => "panic",
            Self::NoDisplay => "no-display",
        }
    }

    /// Whether this scenario needs the `deliberate-panic` kernel.
    pub const fn needs_panic_kernel(self) -> bool {
        matches!(self, Self::Panic)
    }

    /// Judge the serial log so far.
    fn judge(self, log: &str) -> Verdict {
        if !log.contains(markers::KERNEL_ENTERED) {
            return Verdict::Pending;
        }
        match self {
            Self::Boot => {
                if log.contains(markers::PANIC) {
                    Verdict::Fail("kernel panicked".into())
                } else if log.contains(markers::BOOT_DEGRADED) {
                    Verdict::Fail("kernel reported BOOT DEGRADED".into())
                } else if log.contains(markers::BOOT_OK) {
                    Verdict::Pass
                } else {
                    Verdict::Pending
                }
            }
            Self::NoDisplay => {
                if log.contains(markers::PANIC) {
                    Verdict::Fail("kernel panicked".into())
                } else if log.contains(markers::BOOT_OK) {
                    Verdict::Fail("BOOT OK without a display adapter; expected degraded".into())
                } else if log.contains(markers::BOOT_DEGRADED) {
                    if log.contains(markers::GRAPHICS_UNAVAILABLE) {
                        Verdict::Pass
                    } else {
                        Verdict::Fail("degraded, but no graphics-unavailable report".into())
                    }
                } else {
                    Verdict::Pending
                }
            }
            Self::Panic => {
                if log.contains(markers::BOOT_OK) || log.contains(markers::BOOT_DEGRADED) {
                    return Verdict::Fail("kernel finished booting instead of panicking".into());
                }
                let Some(start) = log.find(markers::PANIC) else {
                    return Verdict::Pending;
                };
                let report = &log[start..];
                if !report.contains(markers::PANIC_HALTED) {
                    return Verdict::Pending;
                }
                let missing: Vec<&str> = [
                    (markers::DELIBERATE_PANIC_MESSAGE, "panic message"),
                    (markers::PANIC_LOCATION, "location line"),
                    ("kernel", "kernel source path"),
                    (markers::PANIC_BANNER_DRAWN, "on-screen banner"),
                ]
                .into_iter()
                .filter(|(needle, _)| !report.contains(needle))
                .map(|(_, what)| what)
                .collect();
                if missing.is_empty() {
                    Verdict::Pass
                } else {
                    Verdict::Fail(format!("panic report lacks: {}", missing.join(", ")))
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Pending,
    Pass,
    Fail(String),
}

/// Inputs shared by all scenarios.
#[derive(Debug)]
pub struct Setup {
    pub qemu: PathBuf,
    pub firmware: Firmware,
    pub timeout: Duration,
    pub out_root: PathBuf,
}

/// Boot `image` for `scenario` and check the result. Returns a short summary.
pub fn run(scenario: Scenario, image: &Path, setup: &Setup) -> Result<String, String> {
    let dir = setup.out_root.join(scenario.name());
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let serial_log = dir.join("serial.log");
    let screen_ppm = dir.join("screen.ppm");
    let screen_png = dir.join("screen.png");
    for stale in [&serial_log, &screen_ppm, &screen_png] {
        let _ = std::fs::remove_file(stale);
    }
    let vars = qemu::copy_vars(&setup.firmware, &dir)?;

    let mut cmd = qemu::base_command(&setup.qemu, &setup.firmware, vars.as_deref(), image);
    // Relative to the QEMU working directory (set below), which keeps the
    // chardev argument free of drive letters and spaces.
    cmd.args(["-display", "none", "-monitor", "none"]);
    cmd.args(["-serial", "file:serial.log", "-qmp", "stdio"]);
    if scenario == Scenario::NoDisplay {
        cmd.args(["-vga", "none"]);
    }
    cmd.current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    eprintln!("[smoke:{}] $ {cmd:?}", scenario.name());
    let child = cmd
        .spawn()
        .map_err(|err| format!("failed to start {}: {err}", setup.qemu.display()))?;
    let mut vm = Vm::new(child)?;

    let started = Instant::now();
    let verdict = loop {
        let log = read_lossy(&serial_log);
        match scenario.judge(&log) {
            Verdict::Pending => {}
            done => break done,
        }
        if let Some(status) = vm.child.try_wait().map_err(|err| err.to_string())? {
            // One last look in case the final lines landed just before exit.
            let log = read_lossy(&serial_log);
            break match scenario.judge(&log) {
                Verdict::Pending => Verdict::Fail(format!("QEMU exited early ({status})")),
                done => done,
            };
        }
        if started.elapsed() >= setup.timeout {
            break Verdict::Fail(format!(
                "no success or failure signal within {} s",
                setup.timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let elapsed = started.elapsed();
    let log = read_lossy(&serial_log);

    if let Verdict::Fail(reason) = verdict {
        vm.quit();
        return Err(format!(
            "[smoke:{}] FAIL: {reason}\n--- serial log tail ({}) ---\n{}",
            scenario.name(),
            serial_log.display(),
            tail(&log, 25)
        ));
    }

    let mut summary = format!(
        "[smoke:{}] PASS in {:.1} s (serial log: {})",
        scenario.name(),
        elapsed.as_secs_f64(),
        serial_log.display()
    );
    if scenario != Scenario::NoDisplay {
        let result = vm
            .screendump(&screen_ppm)
            .and_then(|image| check_screen(scenario, &image, &log).map(|()| image))
            .and_then(|image| picture::save_png(&image, &screen_png).map(|()| image));
        match result {
            Ok(image) => {
                summary.push_str(&format!(
                    "\n[smoke:{}] screenshot {}x{} checked: {}",
                    scenario.name(),
                    image.width,
                    image.height,
                    screen_png.display()
                ));
                let _ = std::fs::remove_file(&screen_ppm);
            }
            Err(err) => {
                vm.quit();
                return Err(format!("[smoke:{}] FAIL: {err}", scenario.name()));
            }
        }
    }
    vm.quit();
    Ok(summary)
}

/// Check the screendump against what the kernel should have drawn.
fn check_screen(scenario: Scenario, image: &RgbImage, log: &str) -> Result<(), String> {
    let (width, height) = framebuffer_size(log).ok_or("serial log has no framebuffer line")?;
    if (image.width, image.height) != (width, height) {
        return Err(format!(
            "screenshot is {}x{}, kernel reported {width}x{height}",
            image.width, image.height
        ));
    }
    let expect_pixel = |x: usize, y: usize, color: Rgb, what: &str| {
        let found = image.pixel(x, y);
        if found == Some([color.r, color.g, color.b]) {
            Ok(())
        } else {
            Err(format!(
                "{what} at ({x},{y}) is {found:?}, expected {:?}",
                [color.r, color.g, color.b]
            ))
        }
    };
    let expect_count = |color: Rgb, min: usize, what: &str| {
        let n = count_color(&image.pixels, color);
        if n >= min {
            Ok(())
        } else {
            Err(format!("only {n} {what} pixels, expected at least {min}"))
        }
    };
    expect_pixel(1, 1, theme::BACKGROUND, "background")?;
    expect_count(theme::CARD, 10_000, "card")?;
    expect_count(theme::TITLE, 200, "title text")?;
    expect_count(theme::OK, 100, "status dot")?;
    if scenario == Scenario::Panic {
        expect_pixel(1, height - 1, theme::PANIC_BG, "panic banner")?;
        expect_count(theme::PANIC_TEXT, 200, "panic text")?;
    }
    Ok(())
}

/// Parse `<width>x<height>` after the framebuffer marker.
fn framebuffer_size(log: &str) -> Option<(usize, usize)> {
    let line = log.lines().find(|l| l.starts_with(markers::FRAMEBUFFER))?;
    let dims = line[markers::FRAMEBUFFER.len()..]
        .split_whitespace()
        .next()?;
    let (w, h) = dims.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn read_lossy(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A running QEMU with QMP on its stdio. Killed on drop.
struct Vm {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<String>,
    negotiated: bool,
}

impl Vm {
    fn new(mut child: Child) -> Result<Self, String> {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("QEMU stdout not captured")?;
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            replies,
            negotiated: false,
        })
    }

    /// Send one QMP command and wait for its `return` (or `error`), skipping
    /// the greeting and asynchronous events.
    fn execute(&mut self, command: &str, timeout: Duration) -> Result<String, String> {
        let stdin = self.stdin.as_mut().ok_or("QMP channel closed")?;
        writeln!(stdin, "{command}")
            .and_then(|()| stdin.flush())
            .map_err(|err| format!("QMP write failed: {err}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok(line) if line.contains("\"return\"") => return Ok(line),
                Ok(line) if line.contains("\"error\"") => {
                    return Err(format!("QMP error for {command}: {line}"));
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("QMP timed out waiting for {command}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("QMP channel closed".into());
                }
            }
        }
    }

    fn negotiate(&mut self) -> Result<(), String> {
        if !self.negotiated {
            self.execute(r#"{"execute":"qmp_capabilities"}"#, Duration::from_secs(10))?;
            self.negotiated = true;
        }
        Ok(())
    }

    /// Capture the current display to `path` (PPM) and parse it.
    fn screendump(&mut self, path: &Path) -> Result<RgbImage, String> {
        self.negotiate()?;
        let target = json_string(&path.to_string_lossy());
        self.execute(
            &format!(r#"{{"execute":"screendump","arguments":{{"filename":{target}}}}}"#),
            Duration::from_secs(20),
        )?;
        // The reply is sent once the file is written; allow a short grace
        // period for slow file systems anyway.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let parsed = std::fs::read(path)
                .map_err(|err| format!("cannot read screendump {}: {err}", path.display()))
                .and_then(|data| picture::parse_ppm(&data));
            match parsed {
                Ok(image) => return Ok(image),
                Err(err) if Instant::now() >= deadline => return Err(err),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    /// Ask QEMU to exit; kill it if that does not work promptly.
    fn quit(&mut self) {
        if self.negotiate().is_ok() {
            let _ = self.execute(r#"{"execute":"quit"}"#, Duration::from_secs(5));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Encode `s` as a JSON string literal.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTERED: &str = "[rust-os] kernel entered: x\n";

    #[test]
    fn boot_scenario_verdicts() {
        assert_eq!(Scenario::Boot.judge(""), Verdict::Pending);
        assert_eq!(
            Scenario::Boot.judge(&format!("{ENTERED}[rust-os] BOOT OK: done\n")),
            Verdict::Pass
        );
        assert!(matches!(
            Scenario::Boot.judge(&format!("{ENTERED}[rust-os] KERNEL PANIC\n")),
            Verdict::Fail(_)
        ));
        // Markers before the kernel entered (e.g. echoed by firmware) are ignored.
        assert_eq!(Scenario::Boot.judge("[rust-os] BOOT OK"), Verdict::Pending);
    }

    #[test]
    fn panic_scenario_requires_complete_report() {
        let partial = format!("{ENTERED}[rust-os] KERNEL PANIC\n");
        assert_eq!(Scenario::Panic.judge(&partial), Verdict::Pending);
        let complete = format!(
            "{ENTERED}{}\n{} kernel/src/main.rs:1:1\n[rust-os]   message: {}\n{}\n{}\n",
            markers::PANIC,
            markers::PANIC_LOCATION,
            markers::DELIBERATE_PANIC_MESSAGE,
            markers::PANIC_BANNER_DRAWN,
            markers::PANIC_HALTED
        );
        assert_eq!(Scenario::Panic.judge(&complete), Verdict::Pass);
        let no_banner = complete.replace(markers::PANIC_BANNER_DRAWN, "");
        assert!(matches!(
            Scenario::Panic.judge(&no_banner),
            Verdict::Fail(_)
        ));
        assert!(matches!(
            Scenario::Panic.judge(&format!("{ENTERED}{}\n", markers::BOOT_OK)),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn no_display_scenario_requires_degraded_graphics() {
        let ok = format!(
            "{ENTERED}{} no framebuffer\n{}: x\n",
            markers::GRAPHICS_UNAVAILABLE,
            markers::BOOT_DEGRADED
        );
        assert_eq!(Scenario::NoDisplay.judge(&ok), Verdict::Pass);
        assert!(matches!(
            Scenario::NoDisplay.judge(&format!("{ENTERED}{}\n", markers::BOOT_OK)),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn parses_framebuffer_line() {
        let log = format!("x\n{} 1280x800 stride 1280 px\n", markers::FRAMEBUFFER);
        assert_eq!(framebuffer_size(&log), Some((1280, 800)));
        assert_eq!(framebuffer_size("nothing"), None);
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string(r#"C:\a "b""#), r#""C:\\a \"b\"""#);
    }

    #[test]
    fn scenario_names_round_trip() {
        for s in Scenario::ALL {
            assert_eq!(Scenario::parse(s.name()).unwrap(), vec![s]);
        }
        assert!(Scenario::parse("bogus").is_err());
    }
}
