//! `spin`: CPU-bound worker used to show preemptive scheduling.
//!
//! Sends READY and waits for GO before starting the measured interval.
//! Sends `[START, clock_ns, 0]`, computes without yielding or blocking until
//! STOP, then sends `[DONE, iterations, clock_ns]` and exits 0. A hard
//! deadline fails the test if coordination or preemption stops working.
//! Several spinners can only overlap in time if the kernel preempts them.

#![no_std]
#![no_main]

ulib::entry!(main);

pub const START: u64 = 1;
pub const DONE: u64 = 2;
const HARD_LIMIT_MS: u64 = 10_000;
/// Iterations between clock reads, so nearly all time is spent in pure
/// user-mode computation rather than in system calls.
const BATCH: u64 = 20_000;

fn message(kind: u64, a: u64, b: u64) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[..8].copy_from_slice(&kind.to_le_bytes());
    out[8..16].copy_from_slice(&a.to_le_bytes());
    out[16..].copy_from_slice(&b.to_le_bytes());
    out
}

fn main() -> i32 {
    let Some(channel) = ulib::startup_channel() else {
        return 3;
    };
    if channel.send(&message(0, 0, 0)).is_err() {
        return 2;
    }
    let mut command = [0; 4];
    if channel.recv(&mut command) != Ok(2) || &command[..2] != b"go" {
        return 2;
    }
    let start = ulib::clock_ns();
    if channel.send(&message(START, start, 0)).is_err() {
        return 2;
    }
    let deadline = start + HARD_LIMIT_MS * 1_000_000;
    let mut iterations = 0u64;
    let mut state = 0x2545_f491_4f6c_dd1du64;
    loop {
        for _ in 0..BATCH {
            // xorshift: real work the optimizer cannot remove.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
        }
        iterations += BATCH;
        core::hint::black_box(state);
        let now = ulib::clock_ns();
        match channel.try_recv(&mut command) {
            Ok(4) if command == *b"stop" => {
                return match channel.send(&message(DONE, iterations, now)) {
                    Ok(()) => 0,
                    Err(_) => 2,
                };
            }
            Err(ulib::Error::WouldBlock) => {}
            _ => return 2,
        }
        if now >= deadline {
            let _ = channel.send(&message(DONE, iterations, now));
            return 4;
        }
    }
}
