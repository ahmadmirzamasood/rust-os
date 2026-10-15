//! `init`: the first user process. For now it is the process test driver:
//!
//! 1. **IPC**: starts `echo` with a channel, exchanges messages, asks it to
//!    quit and checks its exit status; then checks that the closed peer is
//!    reported.
//! 2. **Preemption**: starts three CPU-bound `spin` processes that never
//!    yield and checks that all of them were running at the same time (each
//!    started before any finished) and made progress.
//! 3. **Fault containment**: starts `fault` once per crash mode and checks
//!    that each was terminated with the expected CPU exception.
//! 4. **Argument validation**: runs `abuse` and checks it saw every invalid
//!    call fail correctly (exit code 0).
//!
//! Each check prints `init: ok <name>` or `init: FAIL <name>`; the exit code
//! is the number of failures. The kernel reports init's exit status.

#![no_std]
#![no_main]

use ulib::{Channel, ExitStatus, Process, println};

ulib::entry!(main);

const SPINNERS: usize = 3;
const SPIN_START: u64 = 1;
const SPIN_DONE: u64 = 2;

/// Crash modes of `fault` and the exception vector each must end with.
const FAULTS: [(&str, u8); 9] = [
    ("null-read", 14),
    ("kernel-read", 14),
    ("kernel-write", 14),
    ("kernel-exec", 14),
    ("privileged", 13),
    ("port-io", 13),
    ("invalid-opcode", 6),
    ("sse", 6),
    ("stack-overflow", 14),
];

#[derive(Default)]
struct Tally {
    passed: u32,
    failed: u32,
}

impl Tally {
    fn check(&mut self, name: &str, ok: bool) {
        if ok {
            self.passed += 1;
            println!("init: ok {name}");
        } else {
            self.failed += 1;
            println!("init: FAIL {name}");
        }
    }
}

fn main() -> i32 {
    println!("init: started (ABI version {})", ulib::abi_version());
    let mut t = Tally::default();
    ipc(&mut t);
    preemption(&mut t);
    faults(&mut t);
    validation(&mut t);
    println!("init: {} checks passed, {} failed", t.passed, t.failed);
    t.failed as i32
}

fn ipc(t: &mut Tally) {
    let (mine, theirs) = match Channel::pair() {
        Ok(pair) => pair,
        Err(err) => return t.check(&fmt_err("channel pair", err), false),
    };
    let echo = match Process::spawn("echo", Some(theirs)) {
        Ok(p) => p,
        Err(err) => return t.check(&fmt_err("spawn echo", err), false),
    };
    let mut all_ok = true;
    let mut buf = [0u8; 64];
    for i in 0..5u8 {
        let request = [b'p', b'i', b'n', b'g', b' ', b'0' + i];
        let mut expected = *b"echo: ping 0";
        expected[11] = b'0' + i;
        let ok = mine.send(&request).is_ok()
            && mine.recv(&mut buf).is_ok_and(|n| buf[..n] == expected[..]);
        all_ok &= ok;
    }
    t.check("ipc: 5 round trips with echo", all_ok);
    t.check("ipc: send quit", mine.send(b"quit").is_ok());
    t.check(
        "ipc: echo exited with code 0",
        echo.wait() == Ok(ExitStatus::Exited(0)),
    );
    t.check(
        "ipc: send after peer exit fails with PeerClosed",
        mine.send(b"late") == Err(ulib::Error::PeerClosed),
    );
}

fn preemption(t: &mut Tally) {
    let mut spinners: [Option<(Channel, Process)>; SPINNERS] = [None, None, None];
    for slot in &mut spinners {
        let started = Channel::pair().and_then(|(mine, theirs)| {
            Process::spawn("spin", Some(theirs)).map(|process| (mine, process))
        });
        match started {
            Ok(pair) => *slot = Some(pair),
            Err(err) => return t.check(&fmt_err("spawn spin", err), false),
        }
    }
    let mut starts = [0u64; SPINNERS];
    let mut ends = [0u64; SPINNERS];
    let mut iterations = [0u64; SPINNERS];
    let mut protocol_ok = true;
    for (i, (channel, _)) in spinners.iter().flatten().enumerate() {
        match receive3(channel) {
            Some((SPIN_START, start, _)) => starts[i] = start,
            _ => protocol_ok = false,
        }
    }
    for (i, (channel, _)) in spinners.iter().flatten().enumerate() {
        match receive3(channel) {
            Some((SPIN_DONE, count, end)) => {
                iterations[i] = count;
                ends[i] = end;
            }
            _ => protocol_ok = false,
        }
    }
    let mut exits_ok = true;
    for (_, process) in spinners.iter().flatten() {
        exits_ok &= process.wait() == Ok(ExitStatus::Exited(0));
    }
    t.check("preemption: spinner messages", protocol_ok);
    t.check("preemption: spinners exited with code 0", exits_ok);
    let last_start = starts.iter().copied().max().unwrap_or(0);
    let first_end = ends.iter().copied().min().unwrap_or(0);
    println!(
        "init: spinners ran {} / {} / {} batches; last started {} us before the first finished",
        iterations[0] / 20_000,
        iterations[1] / 20_000,
        iterations[2] / 20_000,
        first_end.saturating_sub(last_start) / 1000
    );
    t.check(
        "preemption: all spinners made progress",
        iterations.iter().all(|&n| n > 0),
    );
    t.check(
        "preemption: spinners overlapped in time",
        protocol_ok && last_start < first_end,
    );
}

fn receive3(channel: &Channel) -> Option<(u64, u64, u64)> {
    let mut buf = [0u8; 24];
    let n = channel.recv(&mut buf).ok()?;
    if n != 24 {
        return None;
    }
    let word = |i: usize| {
        let mut w = [0u8; 8];
        w.copy_from_slice(&buf[i * 8..i * 8 + 8]);
        u64::from_le_bytes(w)
    };
    Some((word(0), word(1), word(2)))
}

fn faults(t: &mut Tally) {
    for (mode, vector) in FAULTS {
        let status = Channel::pair().and_then(|(mine, theirs)| {
            let process = Process::spawn("fault", Some(theirs))?;
            mine.send(mode.as_bytes())?;
            process.wait()
        });
        let ok = status == Ok(ExitStatus::Faulted { vector });
        t.check(
            &FmtName::new(format_args!(
                "fault: {mode} terminated with vector {vector} ({status:?})"
            )),
            ok,
        );
    }
}

fn validation(t: &mut Tally) {
    let status = Process::spawn("abuse", None).and_then(|p| p.wait());
    t.check(
        "validation: abuse saw every invalid call rejected",
        status == Ok(ExitStatus::Exited(0)),
    );
    if status != Ok(ExitStatus::Exited(0)) {
        println!("init: abuse ended with {status:?}");
    }
}

fn fmt_err(what: &str, err: ulib::Error) -> FmtName {
    FmtName::new(format_args!("{what}: {}", err.name()))
}

/// A formatted check name.
struct FmtName(ulib::LineBuf);

impl FmtName {
    fn new(args: core::fmt::Arguments<'_>) -> Self {
        let mut buf = ulib::LineBuf::new();
        let _ = core::fmt::Write::write_fmt(&mut buf, args);
        Self(buf)
    }
}

impl core::ops::Deref for FmtName {
    type Target = str;

    fn deref(&self) -> &str {
        core::str::from_utf8(self.0.as_bytes()).unwrap_or("?")
    }
}
