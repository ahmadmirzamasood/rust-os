//! Shared interface between the kernel and user-space components.
//!
//! Everything here is `no_std`, allocation-free and host-testable. While the
//! desktop runs as trusted code inside the kernel, these types are plain Rust
//! calls; they are shaped so they can later cross a system-call boundary.

#![no_std]

/// Version of this interface. Bump on any incompatible change.
pub const ABI_VERSION: u32 = 0;

/// A power transition the desktop may request from the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Restart,
    Sleep,
}

impl PowerAction {
    pub const ALL: [PowerAction; 3] = [Self::Shutdown, Self::Restart, Self::Sleep];
}

/// Power mechanisms the kernel has actually verified on the running platform.
///
/// Every field defaults to `false`; a mechanism is only reported once the
/// kernel has a working, tested implementation for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PowerCapabilities {
    pub shutdown: bool,
    pub restart: bool,
    /// Genuine guest suspend *and* resume. Host-side VM pausing does not count.
    pub suspend_resume: bool,
}

/// Why a power action cannot be offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The kernel has no verified implementation of this mechanism.
    NotImplemented,
}

/// Whether the UI may present a power action as enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(UnavailableReason),
}

impl PowerCapabilities {
    /// Decide whether `action` may be offered. Each action depends only on its
    /// own capability, so working shutdown never implies working sleep.
    pub const fn availability(self, action: PowerAction) -> Availability {
        let supported = match action {
            PowerAction::Shutdown => self.shutdown,
            PowerAction::Restart => self.restart,
            PowerAction::Sleep => self.suspend_resume,
        };
        if supported {
            Availability::Available
        } else {
            Availability::Unavailable(UnavailableReason::NotImplemented)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_available_by_default() {
        let caps = PowerCapabilities::default();
        for action in PowerAction::ALL {
            assert_eq!(
                caps.availability(action),
                Availability::Unavailable(UnavailableReason::NotImplemented)
            );
        }
    }

    #[test]
    fn sleep_stays_unavailable_when_shutdown_and_restart_work() {
        let caps = PowerCapabilities {
            shutdown: true,
            restart: true,
            suspend_resume: false,
        };
        assert_eq!(
            caps.availability(PowerAction::Shutdown),
            Availability::Available
        );
        assert_eq!(
            caps.availability(PowerAction::Restart),
            Availability::Available
        );
        assert_eq!(
            caps.availability(PowerAction::Sleep),
            Availability::Unavailable(UnavailableReason::NotImplemented)
        );
    }

    #[test]
    fn sleep_requires_its_own_capability() {
        let caps = PowerCapabilities {
            suspend_resume: true,
            ..PowerCapabilities::default()
        };
        assert_eq!(
            caps.availability(PowerAction::Sleep),
            Availability::Available
        );
        assert_ne!(
            caps.availability(PowerAction::Shutdown),
            Availability::Available
        );
    }
}
