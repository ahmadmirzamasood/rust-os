//! Only the root boot shell may request transitions. Never discard other apps.
use super::*;

pub fn call(action: u64) -> Result<u64, Error> {
    let _guard = crate::sync::PreemptGuard::new();
    with_current(|t, s| {
        if !cfg!(feature = "shell") || !t.processes[s].display_service {
            return Err(Error::PermissionDenied);
        }
        if action > 2 {
            return Err(Error::InvalidArgument);
        }
        if action != 0
            && t.processes
                .iter()
                .enumerate()
                .any(|(i, p)| i != s && p.state == State::Alive)
        {
            return Err(Error::WouldBlock);
        }
        Ok(())
    })?;
    if action == 0 {
        return Ok(crate::power::capabilities());
    }
    if crate::power::capabilities() == 0 {
        return Err(Error::NotFound);
    }
    // No user thread can run, spawn, or dirty storage between flush and power.
    let deadline = time::uptime_ns().saturating_add(30_000_000_000);
    loop {
        if crate::storage::sync_step().map_err(|_| Error::Io)? {
            break;
        }
        if time::uptime_ns() >= deadline {
            return Err(Error::WouldBlock);
        }
    }
    crate::power::transition(action)
}
