//! Desktop shell model.
//!
//! Pure, host-testable state for the desktop: which icons exist and what they
//! open. Rendering, input and window management live elsewhere and are not
//! implemented yet. This crate must never touch hardware directly.

#![no_std]

use os_abi::{Availability, PowerAction, PowerCapabilities};

/// An icon shown on the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopIcon {
    Computer,
    Files,
    Trash,
}

/// What activating a desktop icon opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchTarget {
    /// Overview of mounted guest volumes.
    VolumeOverview,
    /// The Files browser at the user's home location.
    FilesHome,
    /// The Files browser showing the trash.
    TrashView,
}

impl DesktopIcon {
    /// Icons in their default top-to-bottom order.
    pub const DEFAULT_LAYOUT: [DesktopIcon; 3] = [Self::Computer, Self::Files, Self::Trash];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Computer => "Computer",
            Self::Files => "Files",
            Self::Trash => "Trash",
        }
    }

    pub const fn launch_target(self) -> LaunchTarget {
        match self {
            Self::Computer => LaunchTarget::VolumeOverview,
            Self::Files => LaunchTarget::FilesHome,
            Self::Trash => LaunchTarget::TrashView,
        }
    }
}

/// A power-menu entry as the desktop should present it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerMenuEntry {
    pub action: PowerAction,
    pub label: &'static str,
    pub enabled: bool,
}

/// Build the power menu from what the kernel reports as verified. Entries are
/// always listed so users can see that Sleep exists but is unavailable.
pub fn power_menu(caps: PowerCapabilities) -> [PowerMenuEntry; 3] {
    PowerAction::ALL.map(|action| PowerMenuEntry {
        action,
        label: match action {
            PowerAction::Shutdown => "Shut Down",
            PowerAction::Restart => "Restart",
            PowerAction::Sleep => "Sleep",
        },
        enabled: caps.availability(action) == Availability::Available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_is_computer_files_trash() {
        let labels = DesktopIcon::DEFAULT_LAYOUT.map(DesktopIcon::label);
        assert_eq!(labels, ["Computer", "Files", "Trash"]);
    }

    #[test]
    fn each_icon_opens_a_distinct_target() {
        let [a, b, c] = DesktopIcon::DEFAULT_LAYOUT.map(DesktopIcon::launch_target);
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn sleep_entry_is_listed_but_disabled_without_suspend_resume() {
        let caps = PowerCapabilities {
            shutdown: true,
            restart: true,
            suspend_resume: false,
        };
        let menu = power_menu(caps);
        let sleep = menu
            .iter()
            .find(|e| e.action == PowerAction::Sleep)
            .expect("sleep entry is always listed");
        assert!(!sleep.enabled);
        assert!(
            menu.iter()
                .filter(|e| e.action != PowerAction::Sleep)
                .all(|e| e.enabled)
        );
    }
}
