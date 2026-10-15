//! Kernel entry point.
//!
//! Boot sequence (each step logs to COM1 with the `[rust-os]` prefix):
//!
//! 1. Mask interrupts and bring up the serial port, so every later failure is
//!    reported.
//! 2. Install the kernel's GDT, TSS and IDT, so CPU exceptions (including a
//!    double fault from a stack overflow) are reported instead of resetting
//!    the machine.
//! 3. Validate the boot information: bootloader API version, then copy the
//!    memory map into kernel-owned storage and check it.
//! 4. Take ownership of the framebuffer and validate its geometry and pixel
//!    format. Graphics failure is reported and the kernel continues on serial
//!    only.
//! 5. Set up memory management (frames, kernel mappings, heap, guarded
//!    double-fault stack) and run the memory self-test.
//! 6. Draw the welcome screen, print `[rust-os] BOOT OK` (or
//!    `BOOT DEGRADED`) and halt the CPU.
//!
//! External interrupts stay masked; there is no scheduler yet.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod display;
mod memory;
mod panic;
mod serial;
mod sync;

use core::fmt::Write;

use boot_data::markers;
use boot_data::memory::{MemoryMap, MemoryMapError, MemorySummary};
use bootloader_api::config::{ApiVersion, BootloaderConfig, Mapping};
use bootloader_api::info::MemoryRegion;
use bootloader_api::{BootInfo, entry_point};
use gfx::screens::{StageState, StatusLine, Welcome};
use gfx::{FmtBuf, Layout};

use crate::display::DisplayError;
use crate::sync::TryLock;

/// Boot stack size. The bootloader maps it with an unmapped guard page below.
const KERNEL_STACK_SIZE: u64 = 128 * 1024;

/// What the kernel asks of the bootloader, beyond the defaults.
static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    // Map all physical memory at a bootloader-chosen offset (writable,
    // no-execute). Only `memory::phys` uses it, for page tables and frames.
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config.kernel_stack_size = KERNEL_STACK_SIZE;
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

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

    // --- CPU tables -------------------------------------------------------
    if arch::x86_64::tables::init() {
        let (bottom, top) = arch::x86_64::tables::emergency_stack();
        kprintln!(
            "[rust-os] CPU tables: GDT with TSS, IDT for exception vectors 0-31; \
             double faults on IST1 (bootstrap stack {:#x}..{:#x})",
            bottom,
            top
        );
    }

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
    match boot_info.physical_memory_offset.as_ref() {
        Some(offset) => kprintln!("[rust-os] physical memory mapped at {:#x}", offset),
        None => kprintln!("[rust-os] physical memory NOT mapped (requested in config)"),
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

    // --- Memory management --------------------------------------------------
    let mm_status = init_memory(boot_info, memory.is_ok());
    degraded |= !matches!(
        mm_status,
        MemoryStatus::Ready {
            selftest_ok: true,
            ..
        }
    );

    if let Ok(layout) = &graphics {
        let drawn = draw_welcome(serial_ok, api_ok, &memory, &mm_status, layout);
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
#[cfg(not(any(
    feature = "deliberate-panic",
    feature = "deliberate-page-fault",
    feature = "deliberate-stack-overflow"
)))]
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

/// Smoke-test build: write into the unmapped guard page below the
/// double-fault stack. Expected: a page-fault report naming that guard page,
/// then the panic banner.
#[cfg(feature = "deliberate-page-fault")]
fn finish(_degraded: bool) -> ! {
    let guard = memory::find_guard(markers::DOUBLE_FAULT_STACK_GUARD)
        .expect("double-fault stack guard registered");
    kprintln!("{} write to {:#x}", markers::DELIBERATE_FAULT, guard + 0x10);
    let target = core::ptr::with_exposed_provenance_mut::<u64>((guard + 0x10) as usize);
    // SAFETY: deliberately faulting: the guard page is unmapped, so this
    // write raises a page fault and the kernel stops with a report. The write
    // never completes, so no memory is modified.
    unsafe { target.write_volatile(0xdead_beef) };
    panic!("deliberate page fault did not fault")
}

/// Smoke-test build: recurse until the boot stack runs into its guard page.
/// Expected: a double fault (handled on the IST stack) whose report names the
/// boot stack's guard page, then the panic banner.
#[cfg(feature = "deliberate-stack-overflow")]
fn finish(_degraded: bool) -> ! {
    #[allow(unconditional_recursion)]
    fn recurse(depth: u64) -> u64 {
        let frame = core::hint::black_box([depth; 64]);
        recurse(depth + 1).wrapping_add(core::hint::black_box(frame[0]))
    }
    kprintln!(
        "{} unbounded recursion on the boot stack",
        markers::DELIBERATE_FAULT
    );
    let result = recurse(0);
    panic!("deliberate stack overflow returned {result}")
}

/// Outcome of memory-management setup.
#[derive(Debug, Clone, Copy)]
enum MemoryStatus {
    /// Skipped because the memory map was invalid.
    Skipped,
    Failed(memory::InitError),
    Ready {
        usable_mib: u64,
        free_mib: u64,
        selftest_ok: bool,
    },
}

fn init_memory(boot_info: &BootInfo, map_ok: bool) -> MemoryStatus {
    if !map_ok {
        kprintln!("[rust-os] memory management skipped: memory map invalid");
        return MemoryStatus::Skipped;
    }
    let Some(map) = MEMORY_MAP.try_lock() else {
        return MemoryStatus::Skipped;
    };
    let boot = memory::BootMemory {
        regions: map.regions(),
        physical_memory_offset: boot_info.physical_memory_offset.into_option(),
        kernel_file: (boot_info.kernel_addr, boot_info.kernel_len),
        stack: (boot_info.kernel_stack_bottom, boot_info.kernel_stack_len),
    };
    let report = match memory::init(&boot) {
        Ok(report) => report,
        Err(err) => {
            kprintln!("{} {:?}", markers::MEMORY_INIT_FAILED, err);
            return MemoryStatus::Failed(err);
        }
    };
    let p = report.protections;
    kprintln!(
        "[rust-os] CPU protections: NX {:?}, write-protect {:?}, SMEP {:?}, SMAP {:?}",
        p.no_execute,
        p.write_protect,
        p.smep,
        p.smap
    );
    kprintln!(
        "[rust-os] frames: {} usable frames ({} MiB) at or above 1 MiB; not used: {} KiB \
         below 1 MiB, {} KiB beyond the bitmap, {} B unaligned",
        report.frames.managed_frames,
        report.frames.managed_frames * 4 / 1024,
        report.frames.below_minimum_bytes / 1024,
        report.frames.beyond_capacity_bytes / 1024,
        report.frames.unaligned_bytes
    );
    kprintln!(
        "[rust-os] boot mappings: {} page tables, {} leaf mappings (physical window at {:#x} \
         excluded); {} usable frames were in use and are now reserved; {} W+X, {} user",
        report.page_tables,
        report.boot_leaves,
        report.window_offset,
        report.withdrawn_frames,
        report.writable_executable_leaves,
        report.user_leaves
    );
    kprintln!(
        "[rust-os] kernel region at {:#x}: heap at {:#x} ({} KiB mapped), double-fault stack \
         {:#x}..{:#x}, guard page at {:#x}",
        report.slot_base,
        report.heap_base,
        memory::heap::INITIAL_SIZE / 1024,
        report.double_fault_stack.bottom,
        report.double_fault_stack.top,
        report.double_fault_stack.guard
    );
    kprintln!(
        "{} {} frames free ({} MiB)",
        markers::MEMORY_READY,
        report.free_frames,
        report.free_frames * 4 / 1024
    );

    let outcome = memory::selftest::run(&memory::selftest::Environment {
        regions: map.regions(),
        kernel_file: boot.kernel_file,
    });
    MemoryStatus::Ready {
        usable_mib: report.frames.managed_frames * 4 / 1024,
        free_mib: memory::free_frames() * 4 / 1024,
        selftest_ok: outcome.passed(),
    }
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
    mm_status: &MemoryStatus,
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
    let memory_ok = matches!(
        mm_status,
        MemoryStatus::Ready {
            selftest_ok: true,
            ..
        }
    );
    match (memory, mm_status) {
        (Err(err), _) => {
            let _ = write!(memory_line, "map invalid: {:?}", err);
        }
        (
            Ok(_),
            MemoryStatus::Ready {
                usable_mib,
                free_mib,
                selftest_ok,
            },
        ) => {
            let _ = write!(
                memory_line,
                "{} MiB usable, {} MiB free, self-test {}",
                usable_mib,
                free_mib,
                if *selftest_ok { "passed" } else { "FAILED" }
            );
        }
        (Ok(m), MemoryStatus::Failed(err)) => {
            let _ = write!(
                memory_line,
                "{} MiB; setup failed: {:?}",
                m.usable_bytes / (1024 * 1024),
                err
            );
        }
        (Ok(m), MemoryStatus::Skipped) => {
            let _ = write!(
                memory_line,
                "{} MiB usable; not managed",
                m.usable_bytes / (1024 * 1024)
            );
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
    let all_ok = serial_ok && api_ok && memory.is_ok() && memory_ok;

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
            state: state(memory.is_ok() && memory_ok),
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
