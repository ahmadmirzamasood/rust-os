//! `abuse`: calls the kernel with invalid arguments and checks that each
//! call fails with the documented error instead of crashing anything.
//!
//! Covers unknown call numbers, null/kernel/non-canonical/overflowing/
//! unmapped/read-only pointers, oversized lengths, invalid, closed, reused
//! and wrong-type handles, unknown flags, full queues, closed peers,
//! handle-table exhaustion and unknown programs. Exit code: number of
//! failed checks (0 = all behaved).

#![no_std]
#![no_main]

use os_abi::layout::{STACK_SIZE, STACK_TOP};
use os_abi::syscall as nr;
use ulib::{Channel, Error, ExitStatus, Process, limits, println};

ulib::entry!(main);

/// Read-only data (lands in a non-writable segment).
static READ_ONLY: [u8; 64] = [0x5a; 64];

const KERNEL: u64 = 0xffff_8000_0000_0000;
const NON_CANONICAL: u64 = 0x8000_0000_0000_0000;
const UNMAPPED: u64 = 0x3000_0000;

struct Checks {
    failed: i32,
    total: u32,
}

impl Checks {
    fn expect(&mut self, name: &str, got: ulib::Result<u64>, want: ulib::Result<u64>) {
        self.total += 1;
        if got != want {
            self.failed += 1;
            println!("abuse: FAIL {name}: got {got:?}, expected {want:?}");
        }
    }

    fn expect_err(&mut self, name: &str, got: ulib::Result<u64>, want: Error) {
        self.expect(name, got, Err(want));
    }

    fn fail(&mut self, name: &str, error: Error) {
        self.total += 1;
        self.failed += 1;
        println!("abuse: FAIL {name}: {error:?}");
    }
}

fn raw(number: u64, a: u64, b: u64, c: u64, d: u64) -> ulib::Result<u64> {
    // SAFETY: every pointer this program passes is either invalid on
    // purpose (the kernel must reject it without touching memory) or points
    // into a buffer that no live reference aliases during the call.
    os_abi::decode_result(unsafe { ulib::sys::syscall(number, a, b, c, d) })
}

fn main() -> i32 {
    let mut c = Checks {
        failed: 0,
        total: 0,
    };
    numbers(&mut c);
    pointers(&mut c);
    handles(&mut c);
    channels(&mut c);
    spawning(&mut c);
    println!("abuse: {} checks, {} failed", c.total, c.failed);
    c.failed
}

fn numbers(c: &mut Checks) {
    c.expect_err("call 999", raw(999, 0, 0, 0, 0), Error::UnknownSyscall);
    c.expect_err(
        "call COUNT",
        raw(nr::COUNT, 0, 0, 0, 0),
        Error::UnknownSyscall,
    );
    c.expect_err(
        "call u64::MAX",
        raw(u64::MAX, 1, 2, 3, 4),
        Error::UnknownSyscall,
    );
    c.expect(
        "abi version",
        raw(nr::ABI_VERSION, 0, 0, 0, 0),
        Ok(u64::from(os_abi::ABI_VERSION)),
    );
    let t0 = ulib::clock_ns();
    let t1 = ulib::clock_ns();
    c.expect("clock monotonic", Ok(u64::from(t1 >= t0)), Ok(1));
}

fn pointers(c: &mut Checks) {
    let text = b"abuse: valid write";
    let ok = text.as_ptr() as u64;
    let write = |ptr: u64, len: u64| raw(nr::DEBUG_WRITE, ptr, len, 0, 0);
    c.expect(
        "write valid",
        write(ok, text.len() as u64),
        Ok(text.len() as u64),
    );
    c.expect("write empty", write(ok, 0), Ok(0));
    c.expect("write empty at null", write(0, 0), Ok(0));
    c.expect_err("write null", write(0, 5), Error::BadAddress);
    c.expect_err("write kernel", write(KERNEL, 5), Error::BadAddress);
    c.expect_err(
        "write non-canonical",
        write(NON_CANONICAL, 5),
        Error::BadAddress,
    );
    c.expect_err(
        "write overflowing",
        write(u64::MAX - 2, 16),
        Error::BadAddress,
    );
    c.expect_err("write unmapped", write(UNMAPPED, 4), Error::BadAddress);
    c.expect_err(
        "write ending at user end",
        write(os_abi::layout::USER_END - 2, 4),
        Error::BadAddress,
    );
    let stack_bottom = STACK_TOP - STACK_SIZE;
    c.expect_err(
        "write across stack guard",
        write(stack_bottom - 8, 16),
        Error::BadAddress,
    );
    c.expect_err(
        "write too long",
        write(ok, limits::DEBUG_WRITE_MAX as u64 + 1),
        Error::TooLarge,
    );
    c.expect_err("write huge length", write(ok, u64::MAX), Error::TooLarge);
    c.expect_err(
        "channel_create into kernel",
        raw(nr::CHANNEL_CREATE, KERNEL, 0, 0, 0),
        Error::BadAddress,
    );
    c.expect_err(
        "channel_create into read-only data",
        raw(nr::CHANNEL_CREATE, READ_ONLY.as_ptr() as u64, 0, 0, 0),
        Error::BadAddress,
    );
    c.expect_err(
        "channel_create into code",
        raw(
            nr::CHANNEL_CREATE,
            main as *const () as usize as u64,
            0,
            0,
            0,
        ),
        Error::BadAddress,
    );
    c.expect(
        "read-only data unchanged",
        Ok(u64::from(READ_ONLY.iter().all(|&b| b == 0x5a))),
        Ok(1),
    );
}

fn handles(c: &mut Checks) {
    c.expect_err("close 0", raw(nr::CLOSE, 0, 0, 0, 0), Error::BadHandle);
    c.expect_err(
        "close garbage",
        raw(nr::CLOSE, 0xdead_beef, 0, 0, 0),
        Error::BadHandle,
    );
    c.expect_err(
        "close > u32",
        raw(nr::CLOSE, 1 << 40, 0, 0, 0),
        Error::BadHandle,
    );

    // Fill the handle table: channels come in pairs, so exactly half as
    // many channels fit as handles. Failed creations must leak nothing.
    let mut out = [0u32; 2];
    let mut created = [0u32; limits::HANDLES_PER_PROCESS];
    let mut count = 0;
    let result = loop {
        match raw(nr::CHANNEL_CREATE, out.as_mut_ptr() as u64, 0, 0, 0) {
            Ok(_) if count + 2 <= created.len() => {
                created[count] = out[0];
                created[count + 1] = out[1];
                count += 2;
            }
            other => break other,
        }
    };
    c.expect_err("handle table full", result, Error::NoResources);
    c.expect(
        "handles before exhaustion",
        Ok(count as u64),
        Ok(limits::HANDLES_PER_PROCESS as u64),
    );
    for &h in &created[..count] {
        c.expect(
            "close created",
            raw(nr::CLOSE, u64::from(h), 0, 0, 0),
            Ok(0),
        );
    }
    // Closed handles are dead even though their slots get reused.
    let stale = created[0];
    let fresh = Channel::pair();
    c.expect(
        "create after close",
        fresh.as_ref().map(|_| 0).map_err(|e| *e),
        Ok(0),
    );
    c.expect_err(
        "close stale",
        raw(nr::CLOSE, u64::from(stale), 0, 0, 0),
        Error::BadHandle,
    );
}

fn channels(c: &mut Checks) {
    let (a, b) = match Channel::pair() {
        Ok(pair) => pair,
        Err(err) => return c.fail("channel pair", err),
    };
    let ha = u64::from(a.handle().raw());
    let msg = b"hello";
    let send = |h: u64, ptr: u64, len: u64| raw(nr::CHANNEL_SEND, h, ptr, len, 0);
    c.expect_err("send kernel ptr", send(ha, KERNEL, 4), Error::BadAddress);
    c.expect_err(
        "send too long",
        send(ha, msg.as_ptr() as u64, limits::MESSAGE_MAX as u64 + 1),
        Error::TooLarge,
    );
    c.expect_err(
        "send bad handle",
        send(12345, msg.as_ptr() as u64, 5),
        Error::BadHandle,
    );
    c.expect_err(
        "try_recv empty",
        b.try_recv(&mut [0u8; 8]).map(|n| n as u64),
        Error::WouldBlock,
    );
    c.expect("send", a.send(msg).map(|()| 0), Ok(0));
    let hb = u64::from(b.handle().raw());
    let recv = |ptr: u64, cap: u64, flags: u64| raw(nr::CHANNEL_RECV, hb, ptr, cap, flags);
    c.expect_err("recv into kernel", recv(KERNEL, 64, 1), Error::BadAddress);
    c.expect_err(
        "recv into read-only",
        recv(READ_ONLY.as_ptr() as u64, 64, 1),
        Error::BadAddress,
    );
    let mut small = [0u8; 2];
    c.expect_err(
        "recv too small",
        recv(small.as_mut_ptr() as u64, 2, 1),
        Error::BufferTooSmall,
    );
    let mut buf = [0u8; 16];
    c.expect_err(
        "recv unknown flag",
        recv(buf.as_mut_ptr() as u64, 16, 0x80),
        Error::InvalidArgument,
    );
    // The message survived every failed receive.
    c.expect("recv", b.try_recv(&mut buf).map(|n| n as u64), Ok(5));
    c.expect("recv content", Ok(u64::from(&buf[..5] == msg)), Ok(1));

    for i in 0..limits::CHANNEL_DEPTH {
        c.expect("fill queue", a.send(&[i as u8]).map(|()| 0), Ok(0));
    }
    c.expect_err("queue full", a.send(b"x").map(|()| 0), Error::WouldBlock);
    drop(b);
    c.expect_err(
        "send to closed peer",
        a.send(b"x").map(|()| 0),
        Error::PeerClosed,
    );
    c.expect_err(
        "recv from closed peer",
        a.try_recv(&mut buf).map(|n| n as u64),
        Error::PeerClosed,
    );
}

fn spawning(c: &mut Checks) {
    let spawn = |ptr: u64, len: u64, h: u64| raw(nr::SPAWN, ptr, len, h, 0);
    c.expect_err("spawn kernel name", spawn(KERNEL, 4, 0), Error::BadAddress);
    let missing = b"no-such-program";
    c.expect_err(
        "spawn unknown",
        spawn(missing.as_ptr() as u64, missing.len() as u64, 0),
        Error::NotFound,
    );
    let long = [b'a'; limits::NAME_MAX + 1];
    c.expect_err(
        "spawn long name",
        spawn(long.as_ptr() as u64, long.len() as u64, 0),
        Error::TooLarge,
    );
    c.expect_err(
        "spawn empty name",
        spawn(missing.as_ptr() as u64, 0, 0),
        Error::NotFound,
    );
    let echo = b"echo";
    c.expect_err(
        "spawn bad transfer handle",
        spawn(echo.as_ptr() as u64, 4, 777),
        Error::BadHandle,
    );

    // `echo` without a startup channel exits with code 3.
    let child = Process::spawn("echo", None);
    c.expect(
        "spawn echo",
        child.as_ref().map(|_| 0).map_err(|e| *e),
        Ok(0),
    );
    let Ok(child) = child else { return };
    let hp = u64::from(child.handle().raw());
    c.expect_err(
        "transfer process handle",
        spawn(echo.as_ptr() as u64, 4, hp),
        Error::WrongType,
    );
    c.expect_err(
        "send on process handle",
        raw(nr::CHANNEL_SEND, hp, echo.as_ptr() as u64, 4, 0),
        Error::WrongType,
    );
    c.expect(
        "wait echo",
        child.wait().map(ExitStatus::encode),
        Ok(ExitStatus::Exited(3).encode()),
    );
    c.expect(
        "wait again",
        child.wait().map(ExitStatus::encode),
        Ok(ExitStatus::Exited(3).encode()),
    );
    if let Ok((a, _b)) = Channel::pair() {
        c.expect_err(
            "wait on channel",
            raw(nr::WAIT, u64::from(a.handle().raw()), 0, 0, 0),
            Error::WrongType,
        );
    }
    c.expect_err(
        "wait bad handle",
        raw(nr::WAIT, 999, 0, 0, 0),
        Error::BadHandle,
    );
}
