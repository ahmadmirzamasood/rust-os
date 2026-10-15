//! Kernel entry point.
//!
//! Boot sequence (each step logs to COM1 with the `[rust-os]` prefix):
//!
//! 1. Mask interrupts and bring up the serial port, so every later failure is
//!    reported.
//! 2. Validate the boot information: bootloader API version, then copy the
//!    memory map into kernel-owned storage and check it.
//! 3. Take ownership of the framebuffer, validate its geometry and pixel
//!    format, and draw the welcome screen. Graphics failure is reported and
//!    the kernel continues on serial only.
//! 4. Print `[rust-os] BOOT OK` (or `BOOT DEGRADED`) and halt the CPU.
//!
//! There is no interrupt handling, memory allocator or scheduler yet.

#![no_std]
#![no_main]

mod arch;
mod display;
mod panic;
mod serial;
mod sync;

use core::fmt::Write;

use boot_data::markers;
use boot_data::memory::{MemoryMap, MemoryMapError, MemorySummary};
use bootloader_api::config::ApiVersion;
use bootloader_api::info::MemoryRegion;
use bootloader_api::{BootInfo, entry_point};
use gfx::screens::{StageState, StatusLine, Welcome};
use gfx::{FmtBuf, Layout};

use crate::display::DisplayError;
use crate::sync::TryLock;

entry_point!(kernel_main);

/// Capacity of the kernel's copy of the memory map. QEMU with OVMF reports
/// well under 100 regions; exceeding this is reported as invalid boot data.
const MAX_MEMORY_REGIONS: usize = 512;

/// The validated memory map, preserved for the memory-management phase.
static MEMORY_MAP: TryLock<MemoryMap<MAX_MEMORY_REGIONS>> = TryLock::new(MemoryMap::new());

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    arch::disable_interrupts();
    let serial_ok = serial::init();
    kprintln!(
        "{}: no_std Rust kernel, x86_64, serial COM1 115200 8N1",
        markers::KERNEL_ENTERED
    );
    let _ = os_abi::ABI_VERSION;

    let mut degraded = false;

    // --- Boot information -------------------------------------------------
    let api_ok = check_api_version(boot_info.api_version);
    degraded |= !api_ok;
    kprintln!(
        "[rust-os] kernel image: phys {:#x}, {} KiB; stack {:#x}, {} KiB",
        boot_info.kernel_addr,
        boot_info.kernel_len / 1024,
        boot_info.kernel_stack_bottom,
        boot_info.kernel_stack_len / 1024
    );
    match boot_info.rsdp_addr.as_ref() {
        Some(addr) => kprintln!("[rust-os] ACPI RSDP at phys {:#x}", addr),
        None => kprintln!("[rust-os] ACPI RSDP not provided"),
    }
    // The default bootloader config maps no physical-memory window; memory
    // management sets one up in a later phase.
    match boot_info.physical_memory_offset.as_ref() {
        Some(offset) => kprintln!("[rust-os] physical memory mapped at {:#x}", offset),
        None => kprintln!("[rust-os] physical memory not mapped (not requested)"),
    }

    let memory = load_memory_map(&boot_info.memory_regions);
    match &memory {
        Ok(m) => kprintln!(
            "[rust-os] memory map: {} regions ({} empty), {} usable totalling {} MiB, \
             {} KiB bootloader-reserved, highest address {:#x}",
            m.regions,
            m.empty_regions,
            m.usable_regions,
            m.usable_bytes / (1024 * 1024),
            m.bootloader_bytes / 1024,
            m.highest_address
        ),
        Err(err) => {
            degraded = true;
            kprintln!("[rust-os] memory map INVALID: {:?}", err);
        }
    }

    // --- Graphics -----------------------------------------------------------
    let graphics = match boot_info.framebuffer.take() {
        Some(framebuffer) => display::install(framebuffer).map_err(GraphicsError::Display),
        None => Err(GraphicsError::NoFramebuffer),
    };
    match &graphics {
        Ok(layout) => kprintln!(
            "{} {}x{} stride {} px, {} bytes/pixel, format {:?}",
            markers::FRAMEBUFFER,
            layout.width(),
            layout.height(),
            layout.stride(),
            layout.bytes_per_pixel(),
            layout.format()
        ),
        Err(err) => {
            degraded = true;
            kprintln!(
                "{} {}; continuing with serial diagnostics only",
                markers::GRAPHICS_UNAVAILABLE,
                err
            );
        }
    }
    if let Ok(layout) = &graphics {
        let drawn = draw_welcome(serial_ok, api_ok, &memory, layout);
        if drawn {
            kprintln!("[rust-os] welcome screen drawn");
        } else {
            degraded = true;
            kprintln!("[rust-os] welcome screen NOT drawn: framebuffer busy");
        }
    }

    finish(degraded)
}

/// Report the boot outcome and halt.
#[cfg(not(feature = "deliberate-panic"))]
fn finish(degraded: bool) -> ! {
    if degraded {
        kprintln!(
            "{}: kernel initialized with failures listed above",
            markers::BOOT_DEGRADED
        );
    } else {
        kprintln!(
            "{}: kernel initialized, welcome screen drawn",
            markers::BOOT_OK
        );
    }
    kprintln!("[rust-os] halting CPU (no interrupts or scheduler yet)");
    arch::halt_forever()
}

/// Smoke-test build: exercise the panic path once the welcome screen is up.
#[cfg(feature = "deliberate-panic")]
fn finish(_degraded: bool) -> ! {
    panic!("{}", markers::DELIBERATE_PANIC_MESSAGE)
}

#[derive(Debug)]
enum GraphicsError {
    NoFramebuffer,
    Display(DisplayError),
}

impl core::fmt::Display for GraphicsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoFramebuffer => f.write_str("firmware provided no framebuffer"),
            Self::Display(err) => write!(f, "framebuffer rejected: {err:?}"),
        }
    }
}

/// The kernel was compiled against one `bootloader_api` version; the
/// bootloader that loaded it must speak the same major.minor API.
fn check_api_version(found: ApiVersion) -> bool {
    let expected = ApiVersion::default();
    let ok = found.version_major() == expected.version_major()
        && found.version_minor() == expected.version_minor();
    kprintln!(
        "[rust-os] bootloader API {}.{}.{} (kernel built for {}.{}.{}): {}",
        found.version_major(),
        found.version_minor(),
        found.version_patch(),
        expected.version_major(),
        expected.version_minor(),
        expected.version_patch(),
        if ok { "compatible" } else { "INCOMPATIBLE" }
    );
    ok
}

fn load_memory_map(regions: &[MemoryRegion]) -> Result<MemorySummary, MemoryMapError> {
    match MEMORY_MAP.try_lock() {
        Some(mut map) => map.load(regions),
        // Only reachable if something else holds the map during early boot,
        // which nothing does; report it as an empty map rather than spin.
        None => Err(MemoryMapError::Empty),
    }
}

fn draw_welcome(
    serial_ok: bool,
    api_ok: bool,
    memory: &Result<MemorySummary, MemoryMapError>,
    layout: &Layout,
) -> bool {
    let state = |ok: bool| {
        if ok {
            StageState::Ok
        } else {
            StageState::Degraded
        }
    };

    let mut boot_line = FmtBuf::<64>::new();
    let _ = write!(
        boot_line,
        "UEFI, bootloader API {}",
        if api_ok { "compatible" } else { "MISMATCH" }
    );
    let mut memory_line = FmtBuf::<64>::new();
    match memory {
        Ok(m) => {
            let _ = write!(
                memory_line,
                "{} regions, {} MiB usable",
                m.regions,
                m.usable_bytes / (1024 * 1024)
            );
        }
        Err(err) => {
            let _ = write!(memory_line, "invalid: {:?}", err);
        }
    }
    let mut display_line = FmtBuf::<64>::new();
    let _ = write!(
        display_line,
        "{}x{}, stride {}, {:?}, {} B/px",
        layout.width(),
        layout.height(),
        layout.stride(),
        layout.format(),
        layout.bytes_per_pixel()
    );
    let all_ok = serial_ok && api_ok && memory.is_ok();

    let lines = [
        StatusLine {
            label: "Kernel",
            value: "no_std Rust, x86_64, ring 0",
            state: StageState::Ok,
        },
        StatusLine {
            label: "Boot",
            value: boot_line.as_str(),
            state: state(api_ok),
        },
        StatusLine {
            label: "Serial",
            value: if serial_ok {
                "COM1, 115200 baud"
            } else {
                "no UART detected"
            },
            state: state(serial_ok),
        },
        StatusLine {
            label: "Memory",
            value: memory_line.as_str(),
            state: state(memory.is_ok()),
        },
        StatusLine {
            label: "Display",
            value: display_line.as_str(),
            state: StageState::Ok,
        },
        StatusLine {
            label: "Status",
            value: if all_ok {
                "Kernel initialized"
            } else {
                "Initialized with warnings (see serial log)"
            },
            state: state(all_ok),
        },
    ];
    let welcome = Welcome {
        title: "rust-os",
        subtitle: "An experimental operating system written in Rust",
        lines: &lines,
        footer: "Early boot: no input, files or desktop yet. Diagnostics on COM1.",
    };
    display::with_canvas(|canvas| gfx::screens::draw_welcome(canvas, &welcome)).is_some()
}
