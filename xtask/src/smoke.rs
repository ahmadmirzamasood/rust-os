//! Automated boot smoke test.
//!
//! Each scenario boots an image in headless QEMU with the guest serial port
//! written to a file and QMP (QEMU's control protocol) on stdio. The test
//! watches the serial log for the kernel's markers until an explicit success
//! or failure signal or the timeout. Where the scenario draws, it then takes
//! a QMP `screendump` and checks the pixels against the colours the kernel
//! uses (which also catches red/blue swaps), and saves a PNG.
//!
//! The `input` scenario then drives the guest's PS/2 keyboard and mouse with
//! QMP `input-send-event` (see [`interact`]) and checks the kernel's serial
//! event log and a second screenshot.
//!
//! The `processes` scenario waits for the user-process test driver (`init`)
//! to finish, checks the kernel's own reports of every contained user fault
//! (see [`USER_FAULTS`]), then checks that the kernel is still alive and
//! responsive ([`after_processes`]).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use boot_data::markers;
use gfx::{Rgb, cursor, screens::count_color, theme};

use crate::picture::{self, RgbImage};
use crate::qemu::{self, Firmware};

/// What a smoke run boots and expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Normal image: memory self-test passes, welcome screen, `BOOT OK`.
    Boot,
    /// Image built with `deliberate-panic`: full panic report and banner.
    Panic,
    /// Normal image on a VM without a display adapter: graphics fails,
    /// serial diagnostics continue, `BOOT DEGRADED`.
    NoDisplay,
    /// Image built with `deliberate-page-fault`: a write into an unmapped
    /// guard page is reported as a page fault naming that guard page.
    PageFault,
    /// Image built with `deliberate-stack-overflow`: the boot stack runs
    /// into its guard page; the double fault is handled on its own stack
    /// and reported as a boot-stack overflow.
    StackOverflow,
    /// Image built with `deliberate-interrupt-fault`: the timer interrupt
    /// handler writes into a guard page while the event loop runs; the
    /// report names the IRQ 0 handler as the context.
    InterruptFault,
    /// Normal image, then keyboard and mouse input through QMP: key events,
    /// pointer movement, button clicks, bursts, and the timer heartbeat.
    Input,
    /// Normal image: `init` runs the user-process tests (IPC, preemption of
    /// CPU-bound processes, fault containment, system-call validation); the
    /// kernel reports every user fault, survives them all and still takes
    /// input afterwards.
    Processes,
    Desktop,
}

impl Scenario {
    pub const ALL: [Scenario; 9] = [
        Self::Boot,
        Self::Panic,
        Self::NoDisplay,
        Self::PageFault,
        Self::StackOverflow,
        Self::InterruptFault,
        Self::Input,
        Self::Processes,
        Self::Desktop,
    ];

    pub fn parse(name: &str) -> Result<Vec<Scenario>, String> {
        if name == "all" {
            return Ok(Self::ALL.to_vec());
        }
        Self::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .map(|s| vec![s])
            .ok_or_else(|| {
                format!(
                    "unknown scenario `{name}` (expected boot, panic, no-display, page-fault, \
                     stack-overflow, interrupt-fault, input, processes or all)"
                )
            })
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::Panic => "panic",
            Self::NoDisplay => "no-display",
            Self::PageFault => "page-fault",
            Self::StackOverflow => "stack-overflow",
            Self::InterruptFault => "interrupt-fault",
            Self::Input => "input",
            Self::Processes => "processes",
            Self::Desktop => "desktop",
        }
    }

    /// Kernel feature of the fault-test build this scenario boots, if any.
    pub const fn kernel_feature(self) -> Option<&'static str> {
        match self {
            Self::Boot | Self::NoDisplay | Self::Input | Self::Processes => None,
            Self::Desktop => Some("desktop"),
            Self::Panic => Some("deliberate-panic"),
            Self::PageFault => Some("deliberate-page-fault"),
            Self::StackOverflow => Some("deliberate-stack-overflow"),
            Self::InterruptFault => Some("deliberate-interrupt-fault"),
        }
    }

    /// Whether the scenario ends with the on-screen panic banner.
    const fn expects_banner(self) -> bool {
        matches!(
            self,
            Self::Panic | Self::PageFault | Self::StackOverflow | Self::InterruptFault
        )
    }

    /// Judge the serial log so far.
    fn judge(self, log: &str) -> Verdict {
        if !log.contains(markers::KERNEL_ENTERED) {
            return Verdict::Pending;
        }
        if log.contains(markers::MEMORY_SELFTEST_FAIL) {
            return Verdict::Fail("memory self-test failed".into());
        }
        if log.contains(markers::MEMORY_INIT_FAILED) {
            return Verdict::Fail("memory management failed to initialize".into());
        }
        if log.contains(markers::INPUT_SELFTEST_FAIL) {
            return Verdict::Fail("interrupt/input self-test failed".into());
        }
        if log.contains(markers::INTERRUPTS_FAILED) {
            return Verdict::Fail("interrupts could not be enabled".into());
        }
        if has_line(log, markers::SCHED_SELFTEST_FAIL) || has_line(log, markers::SCHED_FAILED) {
            return Verdict::Fail("scheduler failed to start or its self-test failed".into());
        }
        match self {
            Self::Processes => judge_processes(log),
            Self::Desktop => {
                if log.contains(markers::PANIC)
                    || log.contains(markers::CPU_EXCEPTION)
                    || log.contains("display failed")
                {
                    Verdict::Fail("display or kernel failed".into())
                } else if log.contains("DEMO READY Colors")
                    && log.contains("DEMO READY Input counter")
                    && log.contains("DISPLAY REJECT client 1")
                    && log.contains("DISPLAY REJECT client 2")
                {
                    Verdict::Pass
                } else {
                    Verdict::Pending
                }
            }
            Self::Boot | Self::Input => {
                if log.contains(markers::PANIC) || log.contains(markers::CPU_EXCEPTION) {
                    Verdict::Fail("kernel panicked or took a CPU exception".into())
                } else if log.contains(markers::BOOT_DEGRADED) {
                    Verdict::Fail("kernel reported BOOT DEGRADED".into())
                } else if log.contains(markers::BOOT_OK) {
                    if !log.contains(markers::MEMORY_SELFTEST_PASS) {
                        Verdict::Fail("BOOT OK without a memory self-test pass".into())
                    } else if !log.contains(markers::INPUT_SELFTEST_PASS) {
                        Verdict::Fail("BOOT OK without an input self-test pass".into())
                    } else if !log.contains(markers::UI_READY) {
                        Verdict::Fail("BOOT OK without the interactive screen".into())
                    } else if !has_line(log, markers::SCHED_SELFTEST_PASS) {
                        Verdict::Fail("BOOT OK without a scheduler self-test pass".into())
                    } else {
                        Verdict::Pass
                    }
                } else {
                    Verdict::Pending
                }
            }
            Self::NoDisplay => {
                if log.contains(markers::PANIC) || log.contains(markers::CPU_EXCEPTION) {
                    Verdict::Fail("kernel panicked or took a CPU exception".into())
                } else if log.contains(markers::BOOT_OK) {
                    Verdict::Fail("BOOT OK without a display adapter; expected degraded".into())
                } else if log.contains(markers::BOOT_DEGRADED) {
                    if !log.contains(markers::GRAPHICS_UNAVAILABLE) {
                        Verdict::Fail("degraded, but no graphics-unavailable report".into())
                    } else if !log.contains(markers::MEMORY_SELFTEST_PASS) {
                        Verdict::Fail("memory self-test did not pass without a display".into())
                    } else {
                        Verdict::Pass
                    }
                } else {
                    Verdict::Pending
                }
            }
            Self::Panic => {
                if log.contains(markers::BOOT_OK) || log.contains(markers::BOOT_DEGRADED) {
                    return Verdict::Fail("kernel finished booting instead of panicking".into());
                }
                let Some(report) = complete_panic_report(log) else {
                    return Verdict::Pending;
                };
                require(
                    report,
                    &[
                        (markers::DELIBERATE_PANIC_MESSAGE, "panic message"),
                        (markers::PANIC_LOCATION, "location line"),
                        ("kernel", "kernel source path"),
                        (markers::PANIC_BANNER_DRAWN, "on-screen banner"),
                    ],
                )
            }
            Self::PageFault | Self::StackOverflow | Self::InterruptFault => {
                if self != Self::InterruptFault
                    && (log.contains(markers::BOOT_OK) || log.contains(markers::BOOT_DEGRADED))
                {
                    return Verdict::Fail("kernel finished booting instead of faulting".into());
                }
                let Some(report) = complete_panic_report(log) else {
                    return Verdict::Pending;
                };
                if !log.contains(markers::MEMORY_SELFTEST_PASS) {
                    return Verdict::Fail(
                        "fault happened before the memory self-test passed".into(),
                    );
                }
                let Some(start) = log.find(markers::CPU_EXCEPTION) else {
                    return Verdict::Fail("panic without a CPU exception report".into());
                };
                let exception = &log[start..];
                let expected: &[(&str, &str)] = if self == Self::PageFault {
                    &[
                        ("page fault (#PF)", "page-fault report"),
                        ("page not present, write, kernel mode", "decoded error code"),
                        (markers::FAULT_ADDRESS, "faulting address"),
                        (markers::DOUBLE_FAULT_STACK_GUARD, "guard-page diagnosis"),
                        ("context: normal kernel code", "interrupted context"),
                    ]
                } else if self == Self::InterruptFault {
                    &[
                        ("page fault (#PF)", "page-fault report"),
                        ("page not present, write, kernel mode", "decoded error code"),
                        (markers::FAULT_ADDRESS, "faulting address"),
                        (markers::DOUBLE_FAULT_STACK_GUARD, "guard-page diagnosis"),
                        (markers::TIMER_IRQ_CONTEXT, "timer-interrupt context"),
                        ("interrupts were disabled", "interrupt flag in the handler"),
                    ]
                } else {
                    &[
                        ("double fault (#DF)", "double-fault report"),
                        (markers::FAULT_ADDRESS, "faulting address"),
                        (markers::BOOT_STACK_GUARD, "boot-stack guard diagnosis"),
                    ]
                };
                match require(exception, expected) {
                    Verdict::Pass => {}
                    other => return other,
                }
                if self == Self::InterruptFault && !log.contains(markers::INPUT_SELFTEST_PASS) {
                    return Verdict::Fail("fault happened before interrupts were verified".into());
                }
                if matches!(self, Self::PageFault | Self::InterruptFault)
                    && !fault_address_matches(log)
                {
                    return Verdict::Fail("reported CR2 differs from the address written".into());
                }
                require(
                    report,
                    &[
                        ("unrecoverable CPU exception", "panic message"),
                        (markers::PANIC_BANNER_DRAWN, "on-screen banner"),
                    ],
                )
            }
        }
    }
}

/// Whether some line of `log` starts with `marker`. Used for markers whose
/// text a user process could also print: user output never starts a line
/// (the kernel prefixes it with `[user <name>/<pid>]`).
fn has_line(log: &str, marker: &str) -> bool {
    log.lines().any(|l| l.starts_with(marker))
}

/// Kernel lines starting with `marker`.
fn lines_with<'a>(log: &'a str, marker: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    log.lines().filter(move |l| l.starts_with(marker))
}

/// What the kernel must report for the crashes `init` provokes: (text in a
/// `USER FAULT` line, minimum number of such lines, what it shows).
const USER_FAULTS: [(&str, usize, &str); 7] = [
    ("page not present, read, user mode", 1, "null-pointer read"),
    (
        "protection violation, read, user mode",
        1,
        "read of a mapped kernel page",
    ),
    (
        "protection violation, write, user mode",
        1,
        "write to a mapped kernel page",
    ),
    (
        "protection violation, instruction fetch, user mode",
        1,
        "jump into kernel memory",
    ),
    (
        "general protection fault (#GP, vector 13)",
        2,
        "privileged instruction and port I/O",
    ),
    (
        "invalid opcode (#UD, vector 6)",
        2,
        "ud2 and a disabled SSE instruction",
    ),
    (
        "page not present, write, user mode",
        1,
        "user stack overflow into its guard page",
    ),
];

/// Lowest kernel (upper-half) address.
const KERNEL_HALF: u64 = 0xffff_8000_0000_0000;

fn judge_processes(log: &str) -> Verdict {
    if log.contains(markers::PANIC) || log.contains(markers::CPU_EXCEPTION) {
        return Verdict::Fail("kernel panicked or took a CPU exception".into());
    }
    if has_line(log, markers::BOOT_DEGRADED) {
        return Verdict::Fail("kernel reported BOOT DEGRADED".into());
    }
    if has_line(log, markers::PROCESS_TESTS_FAIL) {
        return Verdict::Fail("the process tests failed (see the init lines)".into());
    }
    if !has_line(log, markers::PROCESS_TESTS_PASS) {
        return Verdict::Pending;
    }
    for (marker, what) in [
        (markers::SCHED_SELFTEST_PASS, "scheduler self-test pass"),
        (markers::PROGRAMS_READY, "program bundle report"),
        (markers::BOOT_OK, "BOOT OK"),
    ] {
        if !has_line(log, marker) {
            return Verdict::Fail(format!("process tests passed without a {what}"));
        }
    }
    let faults: Vec<&str> = lines_with(log, markers::USER_FAULT).collect();
    for (cause, min, what) in USER_FAULTS {
        let n = faults.iter().filter(|l| l.contains(cause)).count();
        if n < min {
            return Verdict::Fail(format!(
                "{n} kernel USER FAULT report(s) for {what} (`{cause}`), expected {min}"
            ));
        }
    }
    // The kernel-memory reads and writes really targeted the kernel half.
    for line in faults.iter().filter(|l| l.contains("protection violation")) {
        if !fault_address(line).is_some_and(|a| a >= KERNEL_HALF) {
            return Verdict::Fail(format!("protection fault not on a kernel address: {line}"));
        }
    }
    let terminated = lines_with(log, markers::PROCESS_EXITED)
        .filter(|l| l.contains("terminated by fault vector"))
        .count();
    if terminated < faults.len() || faults.len() < 9 {
        return Verdict::Fail(format!(
            "{} user faults and {terminated} terminated processes, expected 9 of each",
            faults.len()
        ));
    }
    if !lines_with(log, markers::PROCESS_EXITED).any(|l| l.contains("init (pid 1): exit code 0")) {
        return Verdict::Fail("no kernel report that init exited with code 0".into());
    }
    Verdict::Pass
}

/// The `address 0x...` (CR2) of a user page-fault report.
fn fault_address(line: &str) -> Option<u64> {
    let hex = line.split("address 0x").nth(1)?;
    let digits: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
    u64::from_str_radix(&digits, 16).ok()
}

/// After the process tests: the kernel keeps running (a new timer
/// heartbeat) and the UI thread still handles input.
fn after_processes(vm: &mut Vm, serial_log: &Path, deadline: Instant) -> Result<String, String> {
    let mut w = Watcher::new(serial_log, deadline);
    let log = w.read();
    let pass_at = log
        .find(markers::PROCESS_TESTS_PASS)
        .ok_or("process-test pass marker vanished")?;
    let heartbeat = w.wait(
        "a timer heartbeat after the process tests",
        Duration::from_secs(30),
        |log| log[pass_at..].contains(markers::TIMER_HEARTBEAT),
    )?;
    let from = heartbeat.len();
    send_events(vm, &[key_event("p", true), key_event("p", false)])?;
    let expected = [
        format!("{} press P (scancode 0x19) char 'p'", markers::INPUT_KEY),
        format!("{} release P (scancode 0x19)", markers::INPUT_KEY),
    ];
    w.wait(
        "a key event after the process tests",
        Duration::from_secs(20),
        |log| in_order(log, from, &expected),
    )?;
    let log = w.read();
    let faults = lines_with(&log, markers::USER_FAULT).count();
    let started = lines_with(&log, markers::PROCESS_STARTED).count();
    let init_checks = log
        .lines()
        .filter(|l| l.starts_with(markers::USER_OUTPUT) && l.contains("init: ok "))
        .count();
    Ok(format!(
        "[smoke:processes] {started} processes started, {faults} user faults contained, \
         {init_checks} init checks ok; kernel heartbeat and key input still work afterwards"
    ))
}

/// The text from the panic marker on, once the report is complete.
fn complete_panic_report(log: &str) -> Option<&str> {
    let start = log.find(markers::PANIC)?;
    let report = &log[start..];
    report.contains(markers::PANIC_HALTED).then_some(report)
}

/// Pass if `text` contains every needle, else name what is missing.
fn require(text: &str, needles: &[(&str, &str)]) -> Verdict {
    let missing: Vec<&str> = needles
        .iter()
        .filter(|(needle, _)| !text.contains(needle))
        .map(|(_, what)| *what)
        .collect();
    if missing.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail(format!("report lacks: {}", missing.join(", ")))
    }
}

/// First hexadecimal number after `prefix` on a line starting with it.
fn hex_after(log: &str, prefix: &str) -> Option<u64> {
    let line = log.lines().find(|l| l.starts_with(prefix))?;
    let rest = &line[prefix.len()..];
    let hex = rest.split("0x").nth(1)?;
    let digits: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
    u64::from_str_radix(&digits, 16).ok()
}

/// The deliberate write's target equals the CR2 in the exception report.
fn fault_address_matches(log: &str) -> bool {
    match (
        hex_after(log, markers::DELIBERATE_FAULT),
        hex_after(log, markers::FAULT_ADDRESS),
    ) {
        (Some(target), Some(cr2)) => target == cr2,
        _ => false,
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
    if scenario == Scenario::Desktop {
        desktop_interact(&mut vm, &serial_log, &dir, started + setup.timeout)?;
        vm.quit();
        return Ok(format!(
            "{summary}; two windows, focus/input, move/resize, client failure checked"
        ));
    }
    let mut input = None;
    if scenario == Scenario::Processes {
        let deadline = started + setup.timeout;
        match after_processes(&mut vm, &serial_log, deadline) {
            Ok(report) => summary = format!("{summary}\n{report}"),
            Err(err) => {
                vm.quit();
                return Err(format!(
                    "[smoke:processes] FAIL: {err}\n--- serial log tail ({}) ---\n{}",
                    serial_log.display(),
                    tail(&read_lossy(&serial_log), 30)
                ));
            }
        }
    }
    if scenario == Scenario::Input {
        let deadline = started + setup.timeout;
        match interact(&mut vm, &serial_log, deadline) {
            Ok(report) => {
                summary = format!(
                    "[smoke:input] PASS in {:.1} s (serial log: {})\n{}",
                    started.elapsed().as_secs_f64(),
                    serial_log.display(),
                    report.summary
                );
                input = Some(report);
            }
            Err(err) => {
                vm.quit();
                return Err(format!(
                    "[smoke:input] FAIL: {err}\n--- serial log tail ({}) ---\n{}",
                    serial_log.display(),
                    tail(&read_lossy(&serial_log), 30)
                ));
            }
        }
    }
    let log = read_lossy(&serial_log);
    if scenario != Scenario::NoDisplay {
        let result = vm
            .screendump(&screen_ppm)
            .and_then(|image| check_screen(scenario, &image, &log, input.as_ref()).map(|()| image))
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
fn check_screen(
    scenario: Scenario,
    image: &RgbImage,
    log: &str,
    input: Option<&InputReport>,
) -> Result<(), String> {
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
    if scenario.expects_banner() {
        expect_pixel(1, height - 1, theme::PANIC_BG, "panic banner")?;
        expect_count(theme::PANIC_TEXT, 200, "panic text")?;
        return Ok(());
    }
    expect_count(theme::PANEL, 50_000, "input panel")?;
    // The pointer is the only pure-white shape on screen: all of its fill
    // pixels must be visible, and only inside the arrow's box at the
    // pointer position.
    let pointer = match input {
        Some(report) => report.pointer,
        None => ui_ready(log)
            .map(|ready| ready.pointer)
            .ok_or("serial log has no ui-ready line")?,
    };
    let fill = cursor::fill_pixels();
    let found = count_color(&image.pixels, theme::CURSOR_FILL);
    if found != fill {
        return Err(format!(
            "{found} pointer fill pixels on screen, expected {fill}"
        ));
    }
    let bounds = cursor::bounds(pointer.0 as i32, pointer.1 as i32);
    let mut inside = 0;
    for dy in 0..bounds.h as usize {
        for dx in 0..bounds.w as usize {
            let (x, y) = (pointer.0 as usize + dx, pointer.1 as usize + dy);
            let fill = theme::CURSOR_FILL;
            if image.pixel(x, y) == Some([fill.r, fill.g, fill.b]) {
                inside += 1;
            }
        }
    }
    if inside != fill {
        return Err(format!(
            "pointer not drawn at {},{}: {inside} of {fill} fill pixels there",
            pointer.0, pointer.1
        ));
    }
    if input.is_some() {
        // Pointer rests on the button after the last click: hover colour.
        expect_count(theme::BUTTON_HOVER, 3000, "hovered button")?;
    } else {
        expect_count(theme::BUTTON, 3000, "button")?;
    }
    Ok(())
}

/// Geometry the kernel reports once the interactive screen is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiReady {
    /// x, y, width, height of the test button.
    button: (i64, i64, i64, i64),
    pointer: (i64, i64),
}

/// All integers (optionally negative) in `text`, in order.
fn integers(text: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() || (c == '-' && current.is_empty()) {
            current.push(c);
        } else {
            if let Ok(n) = current.parse() {
                out.push(n);
            }
            current.clear();
        }
    }
    out
}

/// Parse the `ui: ready; button at X,Y WxH; pointer at X,Y` line.
fn ui_ready(log: &str) -> Option<UiReady> {
    let line = log.lines().find(|l| l.starts_with(markers::UI_READY))?;
    let n = integers(&line[markers::UI_READY.len()..]);
    match n[..] {
        [x, y, w, h, px, py, ..] => Some(UiReady {
            button: (x, y, w, h),
            pointer: (px, py),
        }),
        _ => None,
    }
}

/// What the input scenario established, for the screenshot check.
#[derive(Debug)]
struct InputReport {
    /// Where the pointer ended up.
    pointer: (i64, i64),
    summary: String,
}

/// Reads the serial log and remembers when each timer heartbeat line first
/// appeared, to compare guest uptime with host time.
struct Watcher<'a> {
    path: &'a Path,
    deadline: Instant,
    /// (uptime seconds, first seen); `None` if present at the first look.
    heartbeats: Vec<(u64, Option<Instant>)>,
}

impl<'a> Watcher<'a> {
    fn new(path: &'a Path, deadline: Instant) -> Self {
        let mut watcher = Self {
            path,
            deadline,
            heartbeats: Vec::new(),
        };
        let log = read_lossy(path);
        for s in heartbeat_seconds(&log) {
            watcher.heartbeats.push((s, None));
        }
        watcher
    }

    fn read(&mut self) -> String {
        let log = read_lossy(self.path);
        let now = Instant::now();
        for s in heartbeat_seconds(&log) {
            if !self.heartbeats.iter().any(|(seen, _)| *seen == s) {
                self.heartbeats.push((s, Some(now)));
            }
        }
        log
    }

    /// Poll until `done(log)` holds; fail on a kernel panic or exception,
    /// or after `limit` (bounded by the scenario deadline).
    fn wait(
        &mut self,
        what: &str,
        limit: Duration,
        done: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let until = self.deadline.min(Instant::now() + limit);
        loop {
            let log = self.read();
            if log.contains(markers::PANIC) || log.contains(markers::CPU_EXCEPTION) {
                return Err(format!("kernel panicked while waiting for {what}"));
            }
            if done(&log) {
                return Ok(log);
            }
            if Instant::now() >= until {
                return Err(format!("timed out waiting for {what}"));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn last_heartbeat(&self) -> u64 {
        self.heartbeats.iter().map(|(s, _)| *s).max().unwrap_or(0)
    }
}

/// Uptime seconds of every heartbeat line in `log`.
fn heartbeat_seconds(log: &str) -> Vec<u64> {
    log.lines()
        .filter_map(|l| l.strip_prefix(markers::TIMER_HEARTBEAT))
        .filter_map(|rest| rest.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Whether `needles` occur in `log`, in this order, after byte `from`.
fn in_order(log: &str, from: usize, needles: &[String]) -> bool {
    let mut at = from.min(log.len());
    for needle in needles {
        match log[at..].find(needle.as_str()) {
            Some(i) => at += i + needle.len(),
            None => return false,
        }
    }
    true
}

fn key_event(qcode: &str, down: bool) -> String {
    format!(
        r#"{{"type":"key","data":{{"down":{down},"key":{{"type":"qcode","data":"{qcode}"}}}}}}"#
    )
}

fn rel_event(axis: &str, value: i64) -> String {
    format!(r#"{{"type":"rel","data":{{"axis":"{axis}","value":{value}}}}}"#)
}

fn button_event(button: &str, down: bool) -> String {
    format!(r#"{{"type":"btn","data":{{"down":{down},"button":"{button}"}}}}"#)
}

/// Move the pointer by (`dx`, `dy`) in steps of at most 255 counts per
/// axis, one QMP command per step with a short pause.
///
/// QEMU's PS/2 mouse only emits packets (each at most -256..=255 per axis)
/// while its small output queue has room and keeps any remaining motion
/// until the *next* input event, so one large relative event would leave
/// most of the motion pending. Small steps let the guest drain the queue.
fn move_pointer(vm: &mut Vm, dx: i64, dy: i64) -> Result<(), String> {
    let (mut left_x, mut left_y) = (dx, dy);
    while left_x != 0 || left_y != 0 {
        let step_x = left_x.clamp(-255, 255);
        let step_y = left_y.clamp(-255, 255);
        send_events(vm, &[rel_event("x", step_x), rel_event("y", step_y)])?;
        left_x -= step_x;
        left_y -= step_y;
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(())
}

fn send_events(vm: &mut Vm, events: &[String]) -> Result<(), String> {
    vm.negotiate()?;
    vm.execute(
        &format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{}]}}}}"#,
            events.join(",")
        ),
        Duration::from_secs(10),
    )
    .map(|_| ())
}

/// Drive the guest's PS/2 devices through QMP and check its serial log:
///
/// 1. key presses and releases (plain, shifted, extended) are logged in
///    order with the right scancodes and characters;
/// 2. the pointer can be pinned to (0,0) and moved exactly onto the test
///    button, and a left click there is counted;
/// 3. after bursts of keyboard and mouse events, the system still responds
///    to a sentinel key and a second click;
/// 4. with the event loop stalled (F9), the kernel's byte rings really
///    overflow, and afterwards input works again (sentinel, third click);
/// 5. the timer heartbeat keeps running and tracks host time.
fn interact(vm: &mut Vm, serial_log: &Path, deadline: Instant) -> Result<InputReport, String> {
    let mut w = Watcher::new(serial_log, deadline);
    let log = w.read();
    let ready = ui_ready(&log).ok_or("serial log has no parsable ui-ready line")?;
    let (screen_w, screen_h) =
        framebuffer_size(&log).ok_or("serial log has no framebuffer line")?;
    let pin = -(screen_w.max(screen_h) as i64 + 64);
    let (bx, by, bw, bh) = ready.button;
    let target = (bx + bw / 2, by + bh / 2);
    let line = |s: &str| s.to_string();
    let mut report = Vec::new();

    // 1. Keys.
    let from = w.read().len();
    send_events(
        vm,
        &[
            key_event("a", true),
            key_event("a", false),
            key_event("shift", true),
            key_event("b", true),
            key_event("b", false),
            key_event("shift", false),
            key_event("up", true),
            key_event("up", false),
        ],
    )?;
    let expected: Vec<String> = [
        "key press A (scancode 0x1e) char 'a'",
        "key release A (scancode 0x1e)",
        "key press LShift (scancode 0x2a)",
        "key press B (scancode 0x30) char 'B'",
        "key release B (scancode 0x30)",
        "key release LShift (scancode 0x2a)",
        "key press Up (scancode 0xe048)",
        "key release Up (scancode 0xe048)",
    ]
    .map(|s| format!("{} {}", markers::INPUT_KEY, &s["key ".len()..]))
    .to_vec();
    w.wait("8 key events in order", Duration::from_secs(20), |log| {
        in_order(log, from, &expected)
    })?;
    report.push(line(
        "keys: a, Shift+b, Up decoded in order with scancodes and characters",
    ));

    // 2. Pointer: pin to the corner, sync with a right click, move onto the
    //    button, left click.
    let click = |vm: &mut Vm, button: &str| -> Result<(), String> {
        send_events(vm, &[button_event(button, true)])?;
        std::thread::sleep(Duration::from_millis(50));
        send_events(vm, &[button_event(button, false)])
    };
    let pin_and_click = |vm: &mut Vm, w: &mut Watcher<'_>, round: usize| -> Result<(), String> {
        let corner = format!("{} right down at 0,0", markers::INPUT_MOUSE_BUTTON);
        move_pointer(vm, pin, pin)?;
        click(vm, "right")?;
        w.wait("pointer pinned at 0,0", Duration::from_secs(20), |log| {
            log.matches(corner.as_str()).count() >= round
        })?;
        move_pointer(vm, target.0, target.1)?;
        click(vm, "left")?;
        let down = format!(
            "{} left down at {},{}",
            markers::INPUT_MOUSE_BUTTON,
            target.0,
            target.1
        );
        let up = format!(
            "{} left up at {},{}",
            markers::INPUT_MOUSE_BUTTON,
            target.0,
            target.1
        );
        let clicked = format!(
            "{} ({round}) at {},{}",
            markers::UI_CLICK,
            target.0,
            target.1
        );
        w.wait(
            &format!(
                "click {round} on the test button at {},{}",
                target.0, target.1
            ),
            Duration::from_secs(20),
            |log| {
                log.matches(down.as_str()).count() >= round
                    && log.matches(up.as_str()).count() >= round
                    && log.contains(clicked.as_str())
            },
        )?;
        Ok(())
    };
    pin_and_click(vm, &mut w, 1)?;
    report.push(format!(
        "pointer: pinned at 0,0, moved exactly to {},{}, click counted on the button",
        target.0, target.1
    ));

    // 3. Bursts, then a sentinel key and a second click.
    let before = w.read();
    let x_before = before.matches("key press X").count();
    let heartbeat_before = w.last_heartbeat();
    for _ in 0..3 {
        let keys: Vec<String> = (0..40).map(|i| key_event("x", i % 2 == 0)).collect();
        send_events(vm, &keys)?;
    }
    for _ in 0..3 {
        let moves: Vec<String> = (0..60)
            .flat_map(|i| {
                let sign = if i % 2 == 0 { 1 } else { -1 };
                [rel_event("x", 7 * sign), rel_event("y", 5 * sign)]
            })
            .collect();
        send_events(vm, &moves)?;
    }
    let from = w.read().len();
    send_events(vm, &[key_event("z", true), key_event("z", false)])?;
    let sentinel = [
        format!("{} press Z (scancode 0x2c)", markers::INPUT_KEY),
        format!("{} release Z (scancode 0x2c)", markers::INPUT_KEY),
    ];
    w.wait(
        "sentinel key after the burst",
        Duration::from_secs(30),
        |log| in_order(log, from, &sentinel),
    )?;
    pin_and_click(vm, &mut w, 2)?;
    let after = w.read();
    let x_received = after.matches("key press X").count() - x_before;
    report.push(format!(
        "QEMU-paced burst: 60 key presses sent, {x_received} decoded (the rest dropped by \
         QEMU's own PS/2 queue), 360 motion events; responsive afterwards"
    ));

    // 4. Real overflow of the kernel's rings: F9 stalls the event loop while
    //    interrupts keep filling the rings; then the system must recover.
    send_events(vm, &[key_event("f9", true), key_event("f9", false)])?;
    w.wait(
        "the event-loop stall (F9)",
        Duration::from_secs(20),
        |log| log.contains(markers::STALL_BEGIN),
    )?;
    let stall_started = Instant::now();
    // Each command is one mouse packet (3 bytes): 200 of them exceed the
    // 512-byte mouse ring. Keys go in small commands, because QEMU's own
    // queue drops what does not fit: 30 x 12 bytes exceed the 256-byte
    // keyboard ring. Mouse first: it needs the most commands.
    for i in 0..200 {
        let sign = if i % 2 == 0 { 1 } else { -1 };
        send_events(vm, &[rel_event("x", 3 * sign), rel_event("y", 2 * sign)])?;
    }
    for i in 0..30 {
        let keys: Vec<String> = (0..12)
            .map(|j| key_event(if i % 2 == 0 { "c" } else { "v" }, j % 2 == 0))
            .collect();
        send_events(vm, &keys)?;
    }
    let sending = stall_started.elapsed();
    let log = w.wait("the end of the stall", Duration::from_secs(30), |log| {
        log.contains(markers::STALL_END)
    })?;
    let end_line = log
        .lines()
        .find(|l| l.starts_with(markers::STALL_END))
        .unwrap_or_default()
        .to_string();
    let n = integers(&end_line[markers::STALL_END.len()..]);
    let [_, _, kbd_lost, _, _, mouse_lost, ..] = n[..] else {
        return Err(format!("cannot parse stall line: {end_line}"));
    };
    if kbd_lost == 0 || mouse_lost == 0 {
        return Err(format!(
            "rings did not overflow during the stall (sending took {:.1} s): {end_line}",
            sending.as_secs_f64()
        ));
    }
    // Input sent while the rings are still full is dropped by design, so
    // wait until the kernel reports the backlog decoded.
    w.wait(
        "the post-stall backlog to drain",
        Duration::from_secs(30),
        |log| log.contains(markers::STALL_DRAINED),
    )?;
    let from = w.read().len();
    send_events(vm, &[key_event("z", true), key_event("z", false)])?;
    w.wait(
        "sentinel key after the overflow",
        Duration::from_secs(30),
        |log| in_order(log, from, &sentinel),
    )?;
    pin_and_click(vm, &mut w, 3)?;
    report.push(format!(
        "overflow: during an 8 s stall the kernel rings dropped {kbd_lost} keyboard and \
         {mouse_lost} mouse bytes as designed; decoders resynchronized, sentinel key and \
         third click work"
    ));

    // 5. The timer keeps running after the bursts and tracks host time.
    let next = heartbeat_before.max(w.last_heartbeat()) + 1;
    let log = w.wait(
        "a timer heartbeat after the burst",
        Duration::from_secs(25),
        |log| heartbeat_seconds(log).iter().any(|&s| s >= next),
    )?;
    let timed: Vec<(u64, Instant)> = w
        .heartbeats
        .iter()
        .filter_map(|&(s, t)| Some((s, t?)))
        .collect();
    if let (Some(first), Some(last)) = (timed.first(), timed.last())
        && last.0 > first.0
    {
        let guest = (last.0 - first.0) as f64;
        let host = last.1.duration_since(first.1).as_secs_f64();
        let ratio = host / guest;
        if !(0.5..=2.0).contains(&ratio) {
            return Err(format!(
                "guest uptime {guest} s took {host:.2} s of host time (ratio {ratio:.2})"
            ));
        }
        report.push(format!(
            "timer: heartbeats {}..{} s observed over {host:.2} s host time (ratio {ratio:.2})",
            first.0, last.0
        ));
    } else {
        return Err("fewer than two heartbeat lines observed during the run".into());
    }
    if let Some(last) = log
        .lines()
        .rev()
        .find(|l| l.starts_with(markers::TIMER_HEARTBEAT))
    {
        report.push(format!("last heartbeat: {last}"));
    }
    Ok(InputReport {
        pointer: target,
        summary: report
            .iter()
            .map(|r| format!("[smoke:input]   {r}"))
            .collect::<Vec<_>>()
            .join("\n"),
    })
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

/// End-to-end display checks use guest output plus pixels, never readiness alone.
fn desktop_interact(
    vm: &mut Vm,
    serial: &Path,
    dir: &Path,
    deadline: Instant,
) -> Result<(), String> {
    let mut w = Watcher::new(serial, deadline);
    let snap = |vm: &mut Vm, name: &str| -> Result<RgbImage, String> {
        let image = vm.screendump(&dir.join(format!("{name}.ppm")))?;
        picture::save_png(&image, &dir.join(format!("{name}.png")))?;
        Ok(image)
    };
    // Wait for full row presentation: demo readiness can precede the final row.
    let image = loop {
        let image = snap(vm, "two-windows")?;
        if image.pixel(20, 60) == Some([70, 130, 180])
            && image.pixel(100, 100) == Some([180, 100, 65])
        {
            break image;
        }
        if Instant::now() > deadline {
            return Err("two independent window pixels never appeared".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let pin = -(image.width.max(image.height) as i64 + 64);
    let click = |vm: &mut Vm| -> Result<(), String> {
        send_events(vm, &[button_event("left", true)])?;
        std::thread::sleep(Duration::from_millis(60));
        send_events(vm, &[button_event("left", false)])
    };
    move_pointer(vm, pin, pin)?;
    move_pointer(vm, 20, 60)?;
    click(vm)?;
    let from = w.read().len();
    send_events(vm, &[key_event("a", true), key_event("a", false)])?;
    w.wait(
        "Colors received focused input",
        Duration::from_secs(30),
        |log| log[from..].contains("DEMO KEY Colors 30"),
    )?;
    if w.read()[from..].contains("DEMO KEY Input counter 30") {
        return Err("unfocused client received the focused key".into());
    }
    // Drag the now-raised Colors title, exposing its old position.
    move_pointer(vm, 10, -40)?; // 30,20
    send_events(vm, &[button_event("left", true)])?;
    std::thread::sleep(Duration::from_millis(100));
    move_pointer(vm, 400, 230)?;
    send_events(vm, &[button_event("left", false)])?;
    let moved = loop {
        let im = snap(vm, "moved")?;
        if im.pixel(20, 60) == Some([29, 38, 59])
            && im.pixel(450, 300).is_some_and(|p| p[1..] == [130, 180])
        {
            break im;
        }
        if Instant::now() > deadline {
            return Err("move left artifacts or did not redraw".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Resize from the bottom-right grip. The uncovered region must be clean.
    move_pointer(vm, pin, pin)?;
    move_pointer(vm, 650, 400)?;
    send_events(vm, &[button_event("left", true)])?;
    std::thread::sleep(Duration::from_millis(100));
    move_pointer(vm, -150, -60)?;
    send_events(vm, &[button_event("left", false)])?;
    loop {
        let im = snap(vm, "resized")?;
        if im.pixel(620, 380) == Some([29, 38, 59])
            && im.pixel(450, 300).is_some_and(|p| p[1..] == [130, 180])
        {
            break;
        }
        if Instant::now() > deadline {
            return Err("resize left artifacts or did not redraw".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Focus the other independent app, then deliberately panic it with F.
    move_pointer(vm, pin, pin)?;
    move_pointer(vm, 100, 100)?;
    click(vm)?;
    let from = w.read().len();
    send_events(vm, &[key_event("f", true), key_event("f", false)])?;
    w.wait(
        "counter crash contained and peer closed",
        Duration::from_secs(30),
        |log| log[from..].contains("DISPLAY DISCONNECT 2; service alive"),
    )?;
    let from = w.read().len();
    send_events(vm, &[key_event("b", true), key_event("b", false)])?;
    w.wait(
        "surviving application receives input",
        Duration::from_secs(30),
        |log| log[from..].contains("DEMO KEY Colors 48"),
    )?;
    let _ = moved;
    snap(vm, "survivor")?;
    send_events(vm, &[key_event("f4", true), key_event("f4", false)])?;
    w.wait(
        "display service exited for diagnostic handoff",
        Duration::from_secs(30),
        |log| log.contains("process exited: display (pid 1): exit code 0"),
    )?;
    loop {
        let im = snap(vm, "diagnostic-handoff")?;
        if count_color(&im.pixels, theme::CARD) >= 10000
            && count_color(&im.pixels, theme::TITLE) >= 200
            && count_color(&im.pixels, theme::PANEL) >= 50000
        {
            break;
        }
        if Instant::now() > deadline {
            return Err("diagnostic framebuffer was not restored".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let log = w.read();
    if log.contains(markers::PANIC) || log.contains(markers::CPU_EXCEPTION) {
        return Err("kernel failure during desktop checks".into());
    }
    Ok(())
}

/// End-to-end shell checks against a fresh designated disposable RSFS1 volume.
pub fn shell_run(options: &crate::Options) -> Result<(), String> {
    shell_run_inner(options, true)
}

/// Focused power acceptance; independent of multi-stage Files UI stress.
pub fn power_run(options: &crate::Options) -> Result<(), String> {
    shell_run_inner(options, false)
}

fn shell_run_inner(options: &crate::Options, files: bool) -> Result<(), String> {
    let image = crate::build_image(options, crate::KernelVariant::FaultTest("shell"))?;
    let firmware = options.firmware()?;
    let qemu = qemu::program()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let dir = crate::workspace_root()
        .join("target/shell-test")
        .join(format!("{}-{stamp}", options.profile_dir()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let disk = dir.join("DISPOSABLE-rsfs.img");
    crate::storage_test::fresh_disk(&disk)?;
    let vars = qemu::copy_vars(&firmware, &dir)?;
    for boot in 1..=2 {
        let serial = dir.join(format!("boot-{boot}.log"));
        let mut cmd = qemu::base_command(&qemu, &firmware, vars.as_deref(), &image);
        cmd = qemu::enable_power(cmd);
        cmd.args(["-display", "none", "-monitor", "none", "-qmp", "stdio"]);
        cmd.arg("-serial").arg(format!("file:{}", serial.display()));
        cmd.args(["-device", "piix3-ide,id=databus"]);
        cmd.arg("-drive").arg(format!(
            "if=none,id=data,format=raw,cache=writeback,file={}",
            disk.display()
        ));
        cmd.args([
            "-device",
            "ide-hd,drive=data,bus=databus.0,unit=0,serial=RUSTOS-DISPOSABLE",
        ]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut vm = Vm::new(cmd.spawn().map_err(|e| e.to_string())?)?;
        // Includes Files jobs plus a full second firmware boot after restart.
        let deadline = Instant::now() + options.timeout.unwrap_or(Duration::from_secs(600));
        let mut watch = Watcher::new(&serial, deadline);
        watch.wait("shell boot", Duration::from_secs(60), |log| {
            log.contains("SHELL FRAME READY")
        })?;
        let log = watch.read();
        let expected = if boot == 1 {
            "preference=false persistent=true"
        } else {
            "preference=true persistent=true"
        };
        if !log.contains(expected) {
            return Err(format!("preference load mismatch: {}", serial.display()));
        }
        let tap = |vm: &mut Vm, key: &str| -> Result<(), String> {
            send_events(vm, &[key_event(key, true), key_event(key, false)])?;
            std::thread::sleep(Duration::from_millis(160));
            Ok(())
        };
        let snap = |vm: &mut Vm, name: &str| -> Result<RgbImage, String> {
            std::thread::sleep(Duration::from_millis(500));
            let im = vm.screendump(&dir.join(format!("{boot}-{name}.ppm")))?;
            picture::save_png(&im, &dir.join(format!("{boot}-{name}.png")))?;
            Ok(im)
        };
        if boot == 1 {
            // Keyboard launcher: open all built-in applications, leave Settings focused.
            let apps: &[usize] = if files { &[0, 1, 2, 4, 3] } else { &[3] };
            for &app in apps {
                tap(&mut vm, "esc")?;
                for _ in 0..app {
                    tap(&mut vm, "down")?;
                }
                let from = watch.read().len();
                tap(&mut vm, "ret")?;
                watch.wait("application launch", Duration::from_secs(60), |l| {
                    l[from..].contains(&format!("SHELL LAUNCH {app}"))
                })?;
            }
            let im = loop {
                let im = snap(&mut vm, "applications")?;
                if im.pixel(280, 250) == Some([238, 242, 246]) || Instant::now() >= deadline {
                    break im;
                }
            };
            if im.pixel(280, 250) != Some([238, 242, 246]) {
                return Err("Settings content pixels absent".into());
            }
            tap(&mut vm, "ret")?;
            watch.wait("preference synced", Duration::from_secs(60), |l| {
                l.contains("SHELL PREFERENCE true Ok")
            })?;
            let im = loop {
                let im = snap(&mut vm, "saved")?;
                if im.pixel(1200, 400) == Some([65, 42, 52]) || Instant::now() >= deadline {
                    break im;
                }
            };
            if im.pixel(1200, 400) != Some([65, 42, 52]) {
                return Err("accent did not change".into());
            }
            tap(&mut vm, "f1")?;
            watch.wait("minimize", Duration::from_secs(10), |l| {
                l.contains("SHELL MINIMIZE")
            })?;
            tap(&mut vm, "f2")?;
            watch.wait("restore", Duration::from_secs(10), |l| {
                l.contains("SHELL RESTORE")
            })?;
            tap(&mut vm, "tab")?;
            tap(&mut vm, "f3")?;
            watch.wait("close", Duration::from_secs(10), |l| {
                l.contains("SHELL CLOSE")
            })?;
            // Mouse desktop icon launch: pin to origin and click Computer.
            move_pointer(&mut vm, -2000, -2000)?;
            move_pointer(&mut vm, 56, 90)?;
            let from = watch.read().len();
            send_events(&mut vm, &[button_event("left", true)])?;
            std::thread::sleep(Duration::from_millis(80));
            send_events(&mut vm, &[button_event("left", false)])?;
            watch.wait("mouse icon launch", Duration::from_secs(10), |l| {
                l[from..].contains("SHELL LAUNCH 0")
            })?;
            snap(&mut vm, "mouse")?;
        } else {
            let im = loop {
                let im = snap(&mut vm, "restored")?;
                if im.pixel(1200, 400) == Some([65, 42, 52]) || Instant::now() >= deadline {
                    break im;
                }
            };
            if im.pixel(1200, 400) != Some([65, 42, 52]) {
                return Err("saved accent missing after reboot".into());
            }
        }
        if files {
            shell_file_checks(&mut vm, &mut watch, boot, &dir)?;
        }
        if watch.read().contains("SHELL FAILED") || watch.read().contains("KERNEL PANIC") {
            return Err("shell/kernel failure".into());
        }
        // Real guest restart: no QMP system_reset and no host process restart.
        // Esc may first dismiss a Files prompt/viewer; choose launcher explicitly.
        move_pointer(&mut vm, -2000, -2000)?;
        move_pointer(&mut vm, 40, 16)?;
        send_events(&mut vm, &[button_event("left", true)])?;
        std::thread::sleep(Duration::from_millis(100));
        send_events(&mut vm, &[button_event("left", false)])?;
        // Reset menu selection with Esc twice (close then reopen).
        tap(&mut vm, "esc")?;
        tap(&mut vm, "esc")?;
        for _ in 0..6 {
            tap(&mut vm, "down")?;
        }
        let confirm_from = watch.read().len();
        tap(&mut vm, "ret")?;
        watch.wait("power confirmation", Duration::from_secs(60), |l| {
            l[confirm_from..].contains("SHELL POWER CONFIRM 2")
        })?;
        snap(&mut vm, "power-confirmation")?;
        let cancel_from = watch.read().len();
        tap(&mut vm, "esc")?;
        watch.wait("power cancellation", Duration::from_secs(60), |l| {
            l[cancel_from..].contains("SHELL POWER CANCEL")
        })?;
        if watch.read()[cancel_from..].contains("[rust-os] POWER CLEAN") {
            return Err("cancelled request issued a hardware transition".into());
        }
        tap(&mut vm, "esc")?;
        for _ in 0..6 {
            tap(&mut vm, "down")?;
        }
        tap(&mut vm, "ret")?;
        let from = watch.read().len();
        tap(&mut vm, "ret")?;
        watch.wait(
            "guest restart and second firmware boot",
            Duration::from_secs(60),
            |l| {
                l[from..].contains("[rust-os] POWER CLEAN action=2")
                    && l[from..].contains("SHELL FRAME READY")
                    && l[from..].contains("preference=true persistent=true")
            },
        )?;
        tap(&mut vm, "esc")?;
        for _ in 0..5 {
            tap(&mut vm, "down")?;
        }
        tap(&mut vm, "ret")?;
        let from = watch.read().len();
        tap(&mut vm, "ret")?;
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = vm.child.try_wait().map_err(|e| e.to_string())? {
                if !status.success()
                    || !watch.read()[from..].contains("[rust-os] POWER CLEAN action=1")
                {
                    return Err("guest clean shutdown failed".into());
                }
                break;
            }
            if Instant::now() >= end {
                return Err("guest shutdown did not terminate QEMU".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if files {
            crate::storage_test::verify_shell_files(&disk, boot)?;
        }
        crate::storage_test::verify_shell_preferences(&disk)?;
        eprintln!(
            "shell boot {boot}: PASS (independent remount, preference bytes; Files checks={files})"
        );
    }
    eprintln!(
        "shell/power checks PASS (Files UI checks={}): cancel, guest restart, clean shutdown, preferences and independent remount; evidence {}",
        files,
        dir.display()
    );
    Ok(())
}

// Exercise real shell controls, then verify exact persisted bytes independently.
fn shell_file_checks(
    vm: &mut Vm,
    watch: &mut Watcher,
    boot: usize,
    dir: &Path,
) -> Result<(), String> {
    fn tap(vm: &mut Vm, key: &str) -> Result<(), String> {
        send_events(vm, &[key_event(key, true), key_event(key, false)])?;
        std::thread::sleep(Duration::from_millis(140));
        Ok(())
    }
    fn text(vm: &mut Vm, value: &str) -> Result<(), String> {
        for c in value.chars() {
            tap(vm, &c.to_string())?;
        }
        tap(vm, "ret")
    }
    fn open(vm: &mut Vm, watch: &mut Watcher, app: usize) -> Result<(), String> {
        let from = watch.read().len();
        tap(vm, "esc")?;
        for _ in 0..app {
            tap(vm, "down")?;
        }
        tap(vm, "ret")?;
        watch
            .wait("Files view launch", Duration::from_secs(10), |l| {
                l[from..].contains(&format!("SHELL LAUNCH {app}"))
            })
            .map(drop)
    }
    fn finish(vm: &mut Vm, watch: &mut Watcher, app: usize, from: usize) -> Result<(), String> {
        watch.wait("Files operation", Duration::from_secs(120), |l| {
            l[from..].contains(&format!("FILES {app} Complete"))
                || l[from..].contains("partial work kept")
        })?;
        if watch.read()[from..].contains("partial work kept") {
            return Err("Files operation failed; see serial log".into());
        }
        // Redraw after completion before sending another key.
        std::thread::sleep(Duration::from_millis(100));
        let _ = vm;
        Ok(())
    }
    if boot == 1 {
        open(vm, watch, 1)?;
        tap(vm, "n")?;
        let from = watch.read().len();
        text(vm, "docs")?;
        finish(vm, watch, 0, from)?;
        tap(vm, "ret")?; // docs is the first (directory) row.
        tap(vm, "t")?;
        let from = watch.read().len();
        text(vm, "alpha")?;
        finish(vm, watch, 0, from)?;
        tap(vm, "r")?;
        let from = watch.read().len();
        text(vm, "beta")?;
        finish(vm, watch, 0, from)?;
        tap(vm, "ret")?;
        tap(vm, "backspace")?; // read-only text viewer.
        tap(vm, "c")?;
        tap(vm, "backspace")?;
        let from = watch.read().len();
        tap(vm, "v")?;
        finish(vm, watch, 0, from)?;
        tap(vm, "down")?; // root: docs, beta, desktop.pref.
        let from = watch.read().len();
        tap(vm, "delete")?;
        finish(vm, watch, 0, from)?;
        open(vm, watch, 2)?;
    } else {
        open(vm, watch, 2)?;
        let from = watch.read().len();
        tap(vm, "u")?;
        finish(vm, watch, 1, from)?;
        open(vm, watch, 1)?;
        tap(vm, "down")?;
        // Explicit confirmation, including a cancellation that must not delete.
        tap(vm, "d")?;
        tap(vm, "esc")?;
        let from = watch.read().len();
        tap(vm, "delete")?;
        finish(vm, watch, 0, from)?;
        open(vm, watch, 2)?;
        tap(vm, "d")?;
        let from = watch.read().len();
        text(vm, "delete")?;
        finish(vm, watch, 1, from)?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let im = loop {
        std::thread::sleep(Duration::from_millis(500));
        let im = vm.screendump(&dir.join(format!("{boot}-files.ppm")))?;
        if im.pixel(400, 180) == Some([60, 100, 150]) {
            break im;
        }
        if Instant::now() >= deadline {
            return Err("Trash focused pixels absent".into());
        }
    };
    picture::save_png(&im, &dir.join(format!("{boot}-files.png")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTERED: &str = "[rust-os] kernel entered: x\n";

    #[test]
    fn boot_scenario_verdicts() {
        assert_eq!(Scenario::Boot.judge(""), Verdict::Pending);
        let good = format!(
            "{ENTERED}{} (5 checks)\n{} (17 checks)\n{} (9 of 9 checks)\n\
             {} button at 1,2 3x4; pointer at 5,6\n[rust-os] BOOT OK: done\n",
            markers::MEMORY_SELFTEST_PASS,
            markers::INPUT_SELFTEST_PASS,
            markers::SCHED_SELFTEST_PASS,
            markers::UI_READY
        );
        assert_eq!(Scenario::Boot.judge(&good), Verdict::Pass);
        let no_sched = good.replace(markers::SCHED_SELFTEST_PASS, "x");
        assert!(matches!(Scenario::Boot.judge(&no_sched), Verdict::Fail(_)));
        let sched_failed = format!("{good}{} (8 of 9)\n", markers::SCHED_SELFTEST_FAIL);
        assert!(matches!(
            Scenario::Boot.judge(&sched_failed),
            Verdict::Fail(_)
        ));
        assert_eq!(Scenario::Input.judge(&good), Verdict::Pass);
        // Input self-test missing or failed.
        let no_input = good.replace(markers::INPUT_SELFTEST_PASS, "x");
        assert!(matches!(Scenario::Boot.judge(&no_input), Verdict::Fail(_)));
        let failed = format!("{good}{} (1 of 17)\n", markers::INPUT_SELFTEST_FAIL);
        assert!(matches!(Scenario::Boot.judge(&failed), Verdict::Fail(_)));
        let no_ui = good.replace(markers::UI_READY, "x");
        assert!(matches!(Scenario::Boot.judge(&no_ui), Verdict::Fail(_)));
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
    fn boot_scenario_requires_memory_self_test() {
        let no_selftest = format!("{ENTERED}{}: done\n", markers::BOOT_OK);
        assert!(matches!(
            Scenario::Boot.judge(&no_selftest),
            Verdict::Fail(_)
        ));
        let failed = format!("{ENTERED}{} 1 of 2\n", markers::MEMORY_SELFTEST_FAIL);
        assert!(matches!(Scenario::Boot.judge(&failed), Verdict::Fail(_)));
        let exception = format!(
            "{ENTERED}{} (2 checks)\n{} page fault\n",
            markers::MEMORY_SELFTEST_PASS,
            markers::CPU_EXCEPTION
        );
        assert!(matches!(Scenario::Boot.judge(&exception), Verdict::Fail(_)));
    }

    fn fault_log(exception: &str, details: &str) -> String {
        format!(
            "{ENTERED}{pass} (9 checks)\n{deliberate} write to 0xffff804000000010\n\
             {exc} {exception}\n{details}\n{panic}\n\
             [rust-os]   message: unrecoverable CPU exception: x\n{banner}\n{halted}\n",
            pass = markers::MEMORY_SELFTEST_PASS,
            deliberate = markers::DELIBERATE_FAULT,
            exc = markers::CPU_EXCEPTION,
            panic = markers::PANIC,
            banner = markers::PANIC_BANNER_DRAWN,
            halted = markers::PANIC_HALTED,
        )
    }

    #[test]
    fn page_fault_scenario_checks_report_and_address() {
        let details = format!(
            "[rust-os]   cause: page not present, write, kernel mode\n\
             {} normal kernel code (not an interrupt handler); interrupts were enabled\n\
             {} 0xffff804000000010\n{} {}",
            markers::FAULT_CONTEXT,
            markers::FAULT_ADDRESS,
            markers::FAULT_DIAGNOSIS,
            markers::DOUBLE_FAULT_STACK_GUARD
        );
        let good = fault_log("page fault (#PF), vector 14", &details);
        assert_eq!(Scenario::PageFault.judge(&good), Verdict::Pass);
        // The same fault from normal code is not the interrupt-fault case.
        assert!(matches!(
            Scenario::InterruptFault.judge(&good),
            Verdict::Fail(_)
        ));
        let wrong_address = good.replace(
            "0xffff804000000010\n[rust-os]   diag",
            "0xffff804000000018\n[rust-os]   diag",
        );
        assert!(matches!(
            Scenario::PageFault.judge(&wrong_address),
            Verdict::Fail(_)
        ));
        let no_diagnosis = good.replace(markers::DOUBLE_FAULT_STACK_GUARD, "nothing known");
        assert!(matches!(
            Scenario::PageFault.judge(&no_diagnosis),
            Verdict::Fail(_)
        ));
        // A double fault is not what this scenario expects.
        assert!(matches!(
            Scenario::StackOverflow.judge(&good),
            Verdict::Fail(_)
        ));
        // Incomplete report: still pending.
        let partial = good.split(markers::PANIC_HALTED).next().unwrap().to_owned();
        assert_eq!(Scenario::PageFault.judge(&partial), Verdict::Pending);
    }

    #[test]
    fn stack_overflow_scenario_needs_double_fault_on_boot_stack_guard() {
        let details = format!(
            "{} 0xffff800000000ff8\n{} {}",
            markers::FAULT_ADDRESS,
            markers::FAULT_DIAGNOSIS,
            markers::BOOT_STACK_GUARD
        );
        let good = fault_log("double fault (#DF), vector 8", &details);
        assert_eq!(Scenario::StackOverflow.judge(&good), Verdict::Pass);
        let other_guard = good.replace(markers::BOOT_STACK_GUARD, "no known guard page involved");
        assert!(matches!(
            Scenario::StackOverflow.judge(&other_guard),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(hex_after("ab 0x1f zz\n", "ab"), Some(0x1f));
        assert_eq!(hex_after("x\nab: 0x00ff\n", "ab:"), Some(0xff));
        assert_eq!(hex_after("ab nothing\n", "ab"), None);
    }

    #[test]
    fn no_display_scenario_requires_degraded_graphics() {
        let ok = format!(
            "{ENTERED}{} no framebuffer\n{} (3 checks)\n{}: x\n",
            markers::GRAPHICS_UNAVAILABLE,
            markers::MEMORY_SELFTEST_PASS,
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
    fn interrupt_fault_scenario_needs_timer_context() {
        let details = format!(
            "[rust-os]   cause: page not present, write, kernel mode\n\
             {} {}; interrupts were disabled (RFLAGS.IF); uptime 9 ticks\n\
             {} 0xffff804000000010\n{} {}",
            markers::FAULT_CONTEXT,
            markers::TIMER_IRQ_CONTEXT,
            markers::FAULT_ADDRESS,
            markers::FAULT_DIAGNOSIS,
            markers::DOUBLE_FAULT_STACK_GUARD
        );
        let without_selftest = fault_log("page fault (#PF), vector 14", &details);
        // BOOT OK before the fault is fine here (it fires from the event loop).
        let good = without_selftest.replacen(
            "\n",
            &format!(
                "\n{} (17 checks)\n{}: x\n",
                markers::INPUT_SELFTEST_PASS,
                markers::BOOT_OK
            ),
            1,
        );
        assert_eq!(Scenario::InterruptFault.judge(&good), Verdict::Pass);
        assert!(matches!(
            Scenario::InterruptFault.judge(&without_selftest),
            Verdict::Fail(_)
        ));
        let enabled = good.replace("interrupts were disabled", "interrupts were enabled");
        assert!(matches!(
            Scenario::InterruptFault.judge(&enabled),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn parses_ui_ready_and_integers() {
        assert_eq!(integers("a -3,4 x12y 7x8;"), vec![-3, 4, 12, 7, 8]);
        let log = format!(
            "x\n{} button at 280,724 180x32; pointer at 639,399\n",
            markers::UI_READY
        );
        assert_eq!(
            ui_ready(&log),
            Some(UiReady {
                button: (280, 724, 180, 32),
                pointer: (639, 399),
            })
        );
        assert_eq!(
            ui_ready(&format!("{} button at 1,2", markers::UI_READY)),
            None
        );
    }

    #[test]
    fn heartbeats_and_ordered_matching() {
        let log = format!(
            "{h} 1 s (TSC clock)\nnoise\n{h} 10 s (TSC clock)\n",
            h = markers::TIMER_HEARTBEAT
        );
        assert_eq!(heartbeat_seconds(&log), vec![1, 10]);
        let needles = ["b".to_string(), "d".to_string()];
        assert!(in_order("abcde", 0, &needles));
        assert!(!in_order("adcbe", 0, &needles), "d must follow b");
        assert!(!in_order("dcba", 0, &needles));
        assert!(!in_order("abcde", 2, &needles), "search starts at `from`");
        assert!(in_order("abcde", 99, &[]));
    }

    #[test]
    fn qmp_input_events_are_well_formed() {
        assert_eq!(
            key_event("a", true),
            r#"{"type":"key","data":{"down":true,"key":{"type":"qcode","data":"a"}}}"#
        );
        assert_eq!(
            rel_event("x", -5),
            r#"{"type":"rel","data":{"axis":"x","value":-5}}"#
        );
        assert_eq!(
            button_event("left", false),
            r#"{"type":"btn","data":{"down":false,"button":"left"}}"#
        );
    }

    #[test]
    fn scenario_names_round_trip() {
        for s in Scenario::ALL {
            assert_eq!(Scenario::parse(s.name()).unwrap(), vec![s]);
        }
        assert!(Scenario::parse("bogus").is_err());
    }

    /// Serial log of a passing `processes` run (abridged real output).
    fn processes_log() -> String {
        let faults = [
            "fault (pid 6): page fault (#PF, vector 14) at rip 0x402cbb; address 0x0: \
             page not present, read, user mode; process terminated",
            "fault (pid 7): page fault (#PF, vector 14) at rip 0x402bf6; address \
             0xffff80000014fc20: protection violation, read, user mode; process terminated",
            "fault (pid 8): page fault (#PF, vector 14) at rip 0x402b11; address \
             0xffff80000014fc20: protection violation, write, user mode; process terminated",
            "fault (pid 9): page fault (#PF, vector 14) at rip 0xffff80000014fc20; address \
             0xffff80000014fc20: protection violation, instruction fetch, user mode; \
             process terminated",
            "fault (pid 10): general protection fault (#GP, vector 13) at rip 0x402a43; \
             process terminated",
            "fault (pid 11): general protection fault (#GP, vector 13) at rip 0x40299f; \
             process terminated",
            "fault (pid 12): invalid opcode (#UD, vector 6) at rip 0x402875; process terminated",
            "fault (pid 13): invalid opcode (#UD, vector 6) at rip 0x402791; process terminated",
            "fault (pid 14): page fault (#PF, vector 14) at rip 0x402ce7; address 0x7ffefe10: \
             page not present, write, user mode; process terminated",
        ];
        let mut log = format!(
            "{ENTERED}{} (9 of 9 checks)\n{} 5 of 5 programs\n[rust-os] BOOT OK: done\n",
            markers::SCHED_SELFTEST_PASS,
            markers::PROGRAMS_READY
        );
        for (i, f) in faults.iter().enumerate() {
            log.push_str(&format!("{} {f}\n", markers::USER_FAULT));
            log.push_str(&format!(
                "{} fault (pid {}): terminated by fault vector 14; ran 2 ms\n",
                markers::PROCESS_EXITED,
                i + 6
            ));
        }
        log.push_str(&format!(
            "{} init (pid 1): exit code 0; ran 1152 ms\n{}: init ran every process test\n",
            markers::PROCESS_EXITED,
            markers::PROCESS_TESTS_PASS
        ));
        log
    }

    #[test]
    fn processes_scenario_passes_on_a_complete_log() {
        let good = processes_log();
        assert_eq!(Scenario::Processes.judge(&good), Verdict::Pass);
        let pending = good.replace(markers::PROCESS_TESTS_PASS, "x");
        assert_eq!(Scenario::Processes.judge(&pending), Verdict::Pending);
    }

    #[test]
    fn processes_scenario_needs_every_kernel_fault_report() {
        let good = processes_log();
        for (cause, _, what) in USER_FAULTS {
            let broken = good.replacen(cause, "something else", 1);
            assert!(
                matches!(Scenario::Processes.judge(&broken), Verdict::Fail(_)),
                "{what}"
            );
        }
        // A "kernel memory" fault on a user address does not count.
        let user_addr = good.replace(
            "address 0xffff80000014fc20: protection violation, read",
            "address 0x500000: protection violation, read",
        );
        assert!(matches!(
            Scenario::Processes.judge(&user_addr),
            Verdict::Fail(_)
        ));
        // Every faulting process must have been reported as terminated.
        let not_terminated = good.replacen("terminated by fault vector", "exit code 0", 1);
        assert!(matches!(
            Scenario::Processes.judge(&not_terminated),
            Verdict::Fail(_)
        ));
        let no_init_exit = good.replace("init (pid 1): exit code 0", "init (pid 1): exit code 3");
        assert!(matches!(
            Scenario::Processes.judge(&no_init_exit),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn processes_scenario_ignores_markers_printed_by_user_code() {
        // A process printing the pass marker does not count: user output
        // never starts a line.
        let spoof = format!(
            "{ENTERED}{} (9 of 9 checks)\n[user evil/3] {}: fake\n",
            markers::SCHED_SELFTEST_PASS,
            markers::PROCESS_TESTS_PASS
        );
        assert_eq!(Scenario::Processes.judge(&spoof), Verdict::Pending);
        let failed = format!(
            "{}{}: init ended with exit code 2\n",
            processes_log(),
            markers::PROCESS_TESTS_FAIL
        );
        assert!(matches!(
            Scenario::Processes.judge(&failed),
            Verdict::Fail(_)
        ));
        let kernel_fault = format!("{}{} page fault\n", processes_log(), markers::CPU_EXCEPTION);
        assert!(matches!(
            Scenario::Processes.judge(&kernel_fault),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn parses_user_fault_addresses() {
        assert_eq!(
            fault_address("x; address 0xffff800000001000: y"),
            Some(0xffff_8000_0000_1000)
        );
        assert_eq!(fault_address("no address here"), None);
    }
}
