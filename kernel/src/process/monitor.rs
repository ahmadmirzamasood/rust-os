//! Read-only, bounded monitoring for the trusted root shell. No kernel addresses
//! or raw process memory cross this boundary. Copy out after releasing TABLE.
use super::{State, current_root, user_error, with_current};
use crate::{memory, memory::user, sched, time};
use os_abi::{
    Error,
    monitor::{Process, SIZE, Snapshot},
};

const _: () = assert!(super::MAX_PROCESSES <= os_abi::monitor::MAX_PROCESSES);

pub fn snapshot(ptr: u64, len: u64, reserved: u64, extra: u64) -> Result<u64, Error> {
    if !cfg!(feature = "shell") || !with_current(|t, slot| Ok(t.processes[slot].display_service))? {
        return Err(Error::UnknownSyscall);
    }
    if len != SIZE as u64 || reserved | extra != 0 {
        return Err(Error::InvalidArgument);
    }
    let root = current_root()?;
    user::check(root, ptr, SIZE as u64, true).map_err(user_error)?;
    let mut info = Snapshot {
        uptime_ns: time::uptime_ns(),
        free_bytes: memory::free_frames().saturating_mul(4096),
        threads: sched::counters().threads as u32,
        ..Snapshot::default()
    };
    with_current(|t, _| {
        for process in t.processes.iter().filter(|p| p.state != State::Free) {
            let target = &mut info.processes[info.count];
            *target = Process {
                pid: process.pid,
                state: if process.state == State::Alive { 1 } else { 2 },
                name_len: process.name.1 as u8,
                handles: process
                    .handles
                    .iter()
                    .filter(|h| h.object.is_some())
                    .count() as u16,
                cpu_ns: process
                    .thread
                    .and_then(sched::stats)
                    .map_or(0, |s| s.runtime_ns),
                name: process.name.0,
            };
            info.count += 1;
        }
        Ok(())
    })?;
    let mut bytes = [0; SIZE];
    info.encode(&mut bytes);
    user::copy_to(root, ptr, &bytes).map_err(user_error)?;
    Ok(SIZE as u64)
}
