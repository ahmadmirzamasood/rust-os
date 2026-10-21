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
//! 6. Program the 8259 PIC (remapped and fully masked right after step 2),
//!    the PIT tick and the PS/2 keyboard and mouse, enable interrupts and
//!    run the input self-test.
//! 7. Start the scheduler (this boot context becomes the `ui` thread, plus
//!    an idle thread), run the scheduler self-test, and validate the boot
//!    program bundle (the bootloader's ramdisk).
//! 8. Draw the welcome screen and the input-debug panel, print
//!    `[rust-os] BOOT OK` (or `BOOT DEGRADED`), start the first user
//!    process (`init`, the process test driver) and enter the event loop:
//!    pointer, clicks and key events on screen and on serial, while user
//!    processes run preemptively alongside it.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod display;
mod display_device;
mod drivers;
mod event_loop;
mod input_events;
mod input_selftest;
mod memory;
mod panic;
mod power;
mod process;
mod programs;
mod sched;
mod serial;
mod storage;
mod sync;
mod syscall;
mod time;
mod ui;

use core::fmt::Write;

use boot_data::markers;
use boot_data::memory::{MemoryMap, MemoryMapError, MemorySummary};
use bootloader_api::config::{ApiVersion, BootloaderConfig, Mapping};
use bootloader_api::info::MemoryRegion;
use bootloader_api::{BootInfo, entry_point};
use gfx::screens::{StageState, StatusLine, Welcome};
use gfx::{FmtBuf, Layout};

use crate::arch::x86_64::{interrupts, pic, tables};
use crate::display::DisplayError;
use crate::drivers::ps2;
use crate::input_events::Pipeline;
use crate::sync::TryLock;
use crate::ui::Ui;

/// Boot stack size. The bootloader maps it with an unmapped guard page below.
const KERNEL_STACK_SIZE: u64 = 128 * 1024;

/// What the kernel asks of the bootloader, beyond the defaults.
static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    // Map all physical memory at a bootloader-chosen offset (writable,
    // no-execute). Only `memory::phys` uses it, for page tables and frames.
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    // Put every bootloader-chosen mapping (kernel image, boot stack, boot
    // information, framebuffer, physical window, ramdisk) in the upper
    // half, leaving the lower half entirely to user processes.
    config.mappings.dynamic_range_start = Some(KERNEL_HALF_START);
    config.kernel_stack_size = KERNEL_STACK_SIZE;
    config
};

/// First address of the upper (kernel) half of the address space.
const KERNEL_HALF_START: u64 = 0xffff_8000_0000_0000;

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

    let mut degraded = false;

    // --- CPU tables -------------------------------------------------------
    if tables::init() {
        let (bottom, top) = tables::emergency_stack();
        kprintln!(
            "[rust-os] CPU tables: GDT with TSS, IDT with 256 gates (exceptions 0-31, \
             interrupt stubs 32-255: {}); double faults on IST1 (bootstrap stack {:#x}..{:#x})",
            if tables::interrupt_gates_installed() {
                "installed"
            } else {
                "LAYOUT CHECK FAILED, interrupts stay off"
            },
            bottom,
            top
        );
    }
    // Remap the 8259 pair away from the exception vectors and mask every
    // line before anything could unmask interrupts.
    pic::init();
    let simd = arch::x86_64::disable_simd();
    kprintln!(
        "[rust-os] x87/SSE/AVX disabled for all code (CR0.EM set; CR4.OSFXSR, OSXSAVE \
         cleared; firmware had SSE {}, XSAVE {}); UMIP {}",
        if simd.sse_was_enabled { "on" } else { "off" },
        if simd.xsave_was_enabled { "on" } else { "off" },
        if simd.umip { "on" } else { "off" }
    );

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

    // --- Interrupts, timer and input ------------------------------------
    let input = init_interrupts();
    degraded |= !input.ok();

    // --- Scheduler and programs ------------------------------------------
    let tasks = init_tasks(
        boot_info,
        matches!(mm_status, MemoryStatus::Ready { .. }) && arch::x86_64::interrupts_enabled(),
    );
    degraded |= !tasks.ok();
    if tasks.ok() {
        storage::init();
        power::init();
    }

    let (screen_w, screen_h) = display_size();
    let pipeline = Pipeline::new(screen_w, screen_h);
    let mut ui = None;
    if let Ok(layout) = &graphics {
        let welcome = WelcomeInfo {
            serial_ok,
            api_ok,
            memory: &memory,
            mm_status: &mm_status,
            input: &input,
            tasks: &tasks,
            layout,
        };
        let pointer = pipeline.pointer().position();
        match Ui::new(*layout, pointer, |canvas| draw_welcome(&welcome, canvas)) {
            Ok(created) => {
                let b = created.button_rect();
                kprintln!(
                    "{} button at {},{} {}x{}; pointer at {},{}",
                    markers::UI_READY,
                    b.x,
                    b.y,
                    b.w,
                    b.h,
                    pointer.0,
                    pointer.1
                );
                let mut created = created;
                if created.render(&event_loop::status(&pipeline)) {
                    kprintln!(
                        "[rust-os] welcome screen drawn: first frame with input panel and pointer"
                    );
                } else {
                    degraded = true;
                    kprintln!("[rust-os] welcome screen NOT drawn: framebuffer busy");
                }
                ui = Some(created);
            }
            Err(err) => {
                degraded = true;
                kprintln!("[rust-os] interactive screen unavailable: {:?}", err);
                if display::with_canvas(|canvas| draw_welcome(&welcome, canvas)).is_some() {
                    kprintln!("[rust-os] welcome screen drawn (no pointer or input panel)");
                } else {
                    kprintln!("[rust-os] welcome screen NOT drawn: framebuffer busy");
                }
            }
        }
    }

    finish(degraded, input, tasks, pipeline, ui)
}

/// Outcome of scheduler and program-bundle setup.
#[derive(Debug, Clone, Copy)]
struct TaskStatus {
    scheduler: bool,
    selftest: Option<sched::selftest::Outcome>,
    programs: Result<usize, programs::ProgramsError>,
}

impl TaskStatus {
    fn ok(&self) -> bool {
        self.scheduler && self.selftest.is_some_and(|s| s.passed()) && self.programs.is_ok()
    }

    /// Whether user processes can be started.
    fn processes(&self) -> bool {
        self.scheduler && self.programs.is_ok()
    }
}

/// Start the scheduler, run its self-test and validate the program bundle.
fn init_tasks(boot_info: &BootInfo, prerequisites: bool) -> TaskStatus {
    let mut status = TaskStatus {
        scheduler: false,
        selftest: None,
        programs: Err(programs::ProgramsError::NoRamdisk),
    };
    if !prerequisites {
        kprintln!(
            "{} needs memory management and enabled interrupts",
            markers::SCHED_FAILED
        );
        return status;
    }
    match sched::init("ui") {
        Ok(()) => {
            status.scheduler = true;
            kprintln!(
                "{} preemptive round-robin, {} ms time slice, one CPU; up to {} threads with \
                 {} KiB kernel stacks; boot context is thread `ui`, plus `idle`",
                markers::SCHED_READY,
                sched::SLICE_NS / 1_000_000,
                sched::MAX_THREADS,
                sched::STACK_PAGES * 4
            );
        }
        Err(err) => {
            kprintln!("{} {:?}", markers::SCHED_FAILED, err);
            return status;
        }
    }
    status.selftest = Some(sched::selftest::run());

    // SAFETY: the bootloader maps the ramdisk at `ramdisk_addr` for
    // `ramdisk_len` bytes and never unmaps it; the kernel never writes it.
    let bundle =
        unsafe { programs::init(boot_info.ramdisk_addr.into_option(), boot_info.ramdisk_len) };
    match bundle {
        Ok(bundle) => {
            let mut valid = 0;
            for entry in bundle.iter() {
                match process::loader::check(entry.data) {
                    Ok(image) => {
                        valid += 1;
                        kprintln!(
                            "[rust-os]   program {}: {} KiB file, {} segments, {} KiB mapped, \
                             entry {:#x}",
                            entry.name,
                            entry.data.len() / 1024,
                            image.segments().len(),
                            image.mapped_bytes() / 1024,
                            image.entry
                        );
                    }
                    Err(err) => kprintln!(
                        "[rust-os]   program {}: REJECTED: {}",
                        entry.name,
                        err.description()
                    ),
                }
            }
            kprintln!(
                "{} {} of {} programs in the boot bundle are valid static executables ({} KiB)",
                markers::PROGRAMS_READY,
                valid,
                bundle.len(),
                boot_info.ramdisk_len / 1024
            );
            status.programs = Ok(valid);
        }
        Err(err) => {
            kprintln!(
                "{} {:?}; user processes cannot be started",
                markers::PROGRAMS_UNAVAILABLE,
                err
            );
            status.programs = Err(err);
        }
    }
    status
}

/// Outcome of interrupt, timer and input setup.
#[derive(Debug, Clone, Copy)]
struct InputStatus {
    gates: bool,
    ps2: ps2::InitReport,
    tick_millihertz: u64,
    selftest: Option<input_selftest::Outcome>,
}

impl InputStatus {
    fn ok(&self) -> bool {
        self.gates
            && self.ps2.controller.is_ok()
            && self.ps2.keyboard.is_ok()
            && self.ps2.mouse.is_ok()
            && self.selftest.is_some_and(|s| s.passed())
    }
}

/// Program the PIT and PS/2 devices, enable interrupts and run the input
/// self-test. Interrupts stay disabled if the IDT has no interrupt gates.
fn init_interrupts() -> InputStatus {
    let rate = time::init();
    pic::unmask(interrupts::TIMER_IRQ);
    let ps2 = ps2::init();
    let describe = |r: &Result<(), ps2::Ps2Error>| match r {
        Ok(()) => "ok",
        Err(_) => "FAILED",
    };
    kprintln!(
        "[rust-os] PS/2: controller {}, keyboard {}, mouse {} ({} stale bytes flushed)",
        describe(&ps2.controller),
        describe(&ps2.keyboard),
        describe(&ps2.mouse),
        ps2.flushed
    );
    for (what, result) in [
        ("controller", ps2.controller),
        ("keyboard", ps2.keyboard),
        ("mouse", ps2.mouse),
    ] {
        if let Err(err) = result {
            kprintln!("[rust-os]   PS/2 {}: {:?}", what, err);
        }
    }
    let gates = tables::interrupt_gates_installed();
    if !gates {
        kprintln!(
            "{} IDT interrupt stubs failed their layout check; interrupts stay disabled",
            markers::INTERRUPTS_FAILED
        );
        return InputStatus {
            gates,
            ps2,
            tick_millihertz: rate.millihertz,
            selftest: None,
        };
    }
    arch::x86_64::enable_interrupts();
    kprintln!(
        "{} 8259 PIC (vectors {}-{}, mask {:#06x}), PIT divisor {} = {}.{:03} Hz, interrupts enabled",
        markers::INTERRUPTS_READY,
        pic::MASTER_OFFSET,
        pic::MASTER_OFFSET + pic::LINES - 1,
        pic::mask(),
        rate.divisor,
        rate.millihertz / 1000,
        rate.millihertz % 1000
    );
    match rate.tsc_hz {
        Some(hz) => kprintln!(
            "[rust-os] clock: TSC calibrated against PIT channel 2 at {}.{:03} MHz (invariant TSC {})",
            hz / 1_000_000,
            (hz / 1000) % 1000,
            if rate.tsc_invariant {
                "reported"
            } else {
                "not reported by CPUID"
            }
        ),
        None => {
            kprintln!("[rust-os] clock: TSC calibration FAILED; uptime falls back to timer ticks")
        }
    }
    let selftest = input_selftest::run(&ps2);
    InputStatus {
        gates,
        ps2,
        tick_millihertz: rate.millihertz,
        selftest: Some(selftest),
    }
}

/// Report the boot outcome, start `init` and run the event loop (or halt
/// if interrupts could not be enabled).
fn report_and_run(degraded: bool, tasks: TaskStatus, pipeline: Pipeline, ui: Option<Ui>) -> ! {
    if degraded {
        kprintln!(
            "{}: kernel initialized with failures listed above",
            markers::BOOT_DEGRADED
        );
    } else {
        kprintln!(
            "{}: kernel initialized, interactive screen up",
            markers::BOOT_OK
        );
    }
    if !arch::x86_64::interrupts_enabled() {
        kprintln!("[rust-os] halting CPU (interrupts unavailable)");
        arch::halt_forever()
    }
    #[cfg(feature = "deliberate-interrupt-fault")]
    arm_interrupt_fault();
    if tasks.processes() {
        let started = if cfg!(feature = "desktop") {
            process::start_display()
        } else {
            process::start_init(INIT_PROGRAM)
        };
        if let Err(err) = started {
            if cfg!(feature = "desktop") {
                kprintln!("[rust-os] display failed to start: {:?}", err);
            } else {
                process::report_start_failure(INIT_PROGRAM, err);
            }
        }
    } else {
        kprintln!(
            "{}: no scheduler or program bundle; {} not started",
            markers::PROCESS_TESTS_FAIL,
            INIT_PROGRAM
        );
    }
    kprintln!("[rust-os] entering event loop");
    event_loop::run(pipeline, ui)
}

/// Smoke-test build: a page fault inside the timer interrupt handler while
/// the event loop runs (it fires only while the loop is idle in `hlt`, so
/// the screen is never mid-update). Expected: a page-fault report whose
/// context names the IRQ 0 handler, then the panic banner.
#[cfg(feature = "deliberate-interrupt-fault")]
fn arm_interrupt_fault() {
    let guard = memory::find_guard(markers::DOUBLE_FAULT_STACK_GUARD)
        .expect("double-fault stack guard registered");
    kprintln!(
        "{} write to {:#x} from the timer interrupt in {} ticks",
        markers::DELIBERATE_FAULT,
        guard + 0x10,
        time::deliberate::FAULT_DELAY_TICKS
    );
    time::deliberate::arm(guard + 0x10);
}

/// Screen size for the pointer, or a nominal size without a display (the
/// pointer is then only reported on serial).
fn display_size() -> (u32, u32) {
    display::layout().map_or((640, 480), |l| (l.width() as u32, l.height() as u32))
}

/// Normal build (and the interrupt-fault test, which faults later from the
/// event loop).
#[cfg(not(any(
    feature = "deliberate-panic",
    feature = "deliberate-page-fault",
    feature = "deliberate-stack-overflow"
)))]
fn finish(
    degraded: bool,
    _input: InputStatus,
    tasks: TaskStatus,
    pipeline: Pipeline,
    ui: Option<Ui>,
) -> ! {
    report_and_run(degraded, tasks, pipeline, ui)
}

/// The first user process.
const INIT_PROGRAM: &str = "init";

/// Smoke-test build: exercise the panic path once the welcome screen is up.
#[cfg(feature = "deliberate-panic")]
fn finish(
    _degraded: bool,
    _input: InputStatus,
    _tasks: TaskStatus,
    _pipeline: Pipeline,
    _ui: Option<Ui>,
) -> ! {
    // Early-failure build: the event loop is never reached, but stays
    // compiled (and linted) like in the normal build.
    let _ = report_and_run as fn(bool, TaskStatus, Pipeline, Option<Ui>) -> !;
    panic!("{}", markers::DELIBERATE_PANIC_MESSAGE)
}

/// Smoke-test build: write into the unmapped guard page below the
/// double-fault stack. Expected: a page-fault report naming that guard page,
/// then the panic banner.
#[cfg(feature = "deliberate-page-fault")]
fn finish(
    _degraded: bool,
    _input: InputStatus,
    _tasks: TaskStatus,
    _pipeline: Pipeline,
    _ui: Option<Ui>,
) -> ! {
    // Early-failure build: the event loop is never reached, but stays
    // compiled (and linted) like in the normal build.
    let _ = report_and_run as fn(bool, TaskStatus, Pipeline, Option<Ui>) -> !;
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
fn finish(
    _degraded: bool,
    _input: InputStatus,
    _tasks: TaskStatus,
    _pipeline: Pipeline,
    _ui: Option<Ui>,
) -> ! {
    // Early-failure build: the event loop is never reached, but stays
    // compiled (and linted) like in the normal build.
    let _ = report_and_run as fn(bool, TaskStatus, Pipeline, Option<Ui>) -> !;
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

/// Everything the welcome card reports.
struct WelcomeInfo<'a> {
    serial_ok: bool,
    api_ok: bool,
    memory: &'a Result<MemorySummary, MemoryMapError>,
    mm_status: &'a MemoryStatus,
    input: &'a InputStatus,
    tasks: &'a TaskStatus,
    layout: &'a Layout,
}

fn draw_welcome(info: &WelcomeInfo<'_>, canvas: &mut gfx::Canvas<'_>) {
    let WelcomeInfo {
        serial_ok,
        api_ok,
        memory,
        mm_status,
        input,
        tasks,
        layout,
    } = *info;
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
    let mut interrupt_line = FmtBuf::<64>::new();
    let _ = write!(
        interrupt_line,
        "8259 PIC, PIT {} Hz tick, TSC clock{}",
        input.tick_millihertz / 1000,
        match input.selftest {
            Some(s) if s.passed() => ", self-test passed",
            Some(_) => ", self-test FAILED",
            None => ", NOT enabled",
        }
    );
    let input_devices_ok =
        input.ps2.keyboard.is_ok() && input.ps2.mouse.is_ok() && input.ps2.controller.is_ok();
    let input_line = match (input.ps2.keyboard.is_ok(), input.ps2.mouse.is_ok()) {
        (true, true) => "PS/2 keyboard and mouse (no USB support)",
        (true, false) => "PS/2 keyboard only; mouse unavailable",
        (false, true) => "PS/2 mouse only; keyboard unavailable",
        (false, false) => "no PS/2 input devices",
    };
    let mut tasks_line = FmtBuf::<64>::new();
    let _ = if tasks.scheduler {
        write!(
            tasks_line,
            "preemptive {} ms slices, self-test {}; {}",
            sched::SLICE_NS / 1_000_000,
            if tasks.selftest.is_some_and(|s| s.passed()) {
                "passed"
            } else {
                "FAILED"
            },
            if tasks.programs.is_ok() {
                "programs ready"
            } else {
                "NO programs"
            }
        )
    } else {
        write!(tasks_line, "scheduler NOT running")
    };
    let all_ok = serial_ok && api_ok && memory.is_ok() && memory_ok && input.ok() && tasks.ok();

    let lines = [
        StatusLine {
            label: "Kernel",
            value: "no_std Rust, x86_64; user processes in ring 3",
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
            label: "Interrupts",
            value: interrupt_line.as_str(),
            state: state(input.gates && input.selftest.is_some_and(|s| s.passed())),
        },
        StatusLine {
            label: "Input",
            value: input_line,
            state: state(input_devices_ok),
        },
        StatusLine {
            label: "Tasks",
            value: tasks_line.as_str(),
            state: state(tasks.ok()),
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
        footer: "No files or desktop yet. Diagnostics and process tests on COM1.",
        reserve_bottom: gfx::screens::input_panel_reserve(ui::PANEL_LINES),
    };
    gfx::screens::draw_welcome(canvas, &welcome);
}
