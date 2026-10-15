//! Kernel memory management: physical frames, kernel mappings, stacks and
//! the heap.
//!
//! # Ownership
//!
//! - **Frames.** A physical frame can be allocated only if the firmware map
//!   says *usable*, it lies at or above 1 MiB, and nothing found at boot uses
//!   it. At [`init`] the kernel walks the *live* page tables (except the
//!   physical-memory window) and withdraws every page-table frame and every
//!   frame any boot-time mapping points to: kernel image, boot stack, boot
//!   information, framebuffer, bootloader trampolines. Those frames stay
//!   reserved forever in this phase. Allocated frames belong to the caller
//!   until freed.
//! - **Page tables.** The bootloader's mappings are *foreign*: never edited.
//!   The kernel claims one unused top-level (PML4) slot, 512 GiB of address
//!   space, and only ever maps, unmaps or re-protects pages inside it. It is
//!   split into the heap, guarded stacks and a scratch area for self-tests.
//!   All page-table edits go through [`mm::paging::Mapper`] while holding
//!   the manager lock, which enforces W^X and never replaces a mapping.
//! - **TLB.** After every edit of a present entry (unmap, protect) and after
//!   every map, the kernel runs `invlpg` for that page before releasing the
//!   lock. One CPU, so no other TLB needs a shootdown; SMP will.
//!
//! # Locks
//!
//! The manager lives in a [`TryLock`]. Nothing here ever spins: a busy lock
//! is reported as [`MemError::Busy`]. Lock order is heap → manager; the
//! manager never allocates from the heap. Exception handlers only read the
//! guard registry, with `try_lock`.

pub mod heap;
mod phys;
pub mod selftest;

use boot_data::markers;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use mm::frames::{FrameBitmap, FrameError, FrameInitError, FrameInitReport};
use mm::paging::{FrameSource, MapError, Mapper, Translation, WalkItem, bits as pte};
use mm::{PAGE_SIZE, Page, PhysAddr, PhysFrame, VirtAddr};

pub use mm::paging::Flags;

use self::phys::PhysWindow;
use crate::arch::x86_64::{self as cpu, bits, invalidate_page, tables};
use crate::sync::TryLock;

/// Frames below this address are never allocated (legacy BIOS area, future
/// SMP trampolines).
pub const MIN_ALLOCATABLE: u64 = 1 << 20;

/// Bitmap words per half (managed / free): 16384 * 64 frames = 4 GiB of
/// physical address space above [`MIN_ALLOCATABLE`]. Usable RAM beyond that
/// is reported and left unused.
const BITMAP_WORDS: usize = 16 * 1024;

const GIB: u64 = 1 << 30;
/// Layout of the kernel's own PML4 slot (offsets from its base).
const HEAP_OFFSET: u64 = 0;
/// Upper bound of the heap's address range; frames are the real limit.
pub const HEAP_MAX: u64 = GIB;
const STACKS_OFFSET: u64 = 256 * GIB;
const SCRATCH_OFFSET: u64 = 384 * GIB;
const SLOT_SIZE: u64 = 512 * GIB;

/// Why a memory operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    /// [`init`] has not succeeded.
    NotInitialized,
    /// The manager lock is held (re-entry from an exception or panic).
    Busy,
    Frames(FrameError),
    Map(MapError),
    /// The address is outside the kernel's own region.
    NotOwned,
    /// A region of the kernel's address space is full.
    AddressSpaceFull,
}

/// Why [`init`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    /// The bootloader did not map physical memory.
    NoPhysicalWindow,
    /// [`init`] ran twice.
    AlreadyInitialized,
    FrameMap(FrameInitError),
    /// No unused upper-half PML4 slot.
    NoFreeSlot,
    /// The physical window does not translate back to itself.
    WindowMismatch,
    /// The CPU lacks the no-execute feature (the boot page tables need it).
    NoExecuteUnsupported,
    Heap(MemError),
    Stack(MemError),
}

/// What the boot information says about memory.
#[derive(Debug)]
pub struct BootMemory<'a> {
    pub regions: &'a [MemoryRegion],
    pub physical_memory_offset: Option<u64>,
    /// Physical range of the kernel ELF file.
    pub kernel_file: (u64, u64),
    /// Virtual `[bottom, bottom + len)` of the boot stack.
    pub stack: (u64, u64),
}

/// State of a CPU protection feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Was already on.
    On,
    /// Turned on by the kernel.
    Enabled,
    /// The CPU does not support it.
    Unsupported,
    /// Supported but left off; the reason is in the report.
    Skipped,
}

#[derive(Debug, Clone, Copy)]
pub struct Protections {
    pub no_execute: Protection,
    pub write_protect: Protection,
    pub smep: Protection,
    pub smap: Protection,
}

/// Summary of [`init`].
#[derive(Debug, Clone, Copy)]
pub struct InitReport {
    pub frames: FrameInitReport,
    pub free_frames: u64,
    pub protections: Protections,
    pub page_tables: u64,
    pub boot_leaves: u64,
    /// Frames marked usable by firmware but in use by boot mappings.
    pub withdrawn_frames: u64,
    pub writable_executable_leaves: u64,
    pub user_leaves: u64,
    pub window_offset: u64,
    pub slot_base: u64,
    pub heap_base: u64,
    pub double_fault_stack: KernelStack,
}

/// A kernel stack with an unmapped guard page directly below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelStack {
    pub guard: u64,
    pub bottom: u64,
    /// Exclusive top; 16-byte aligned. Initial RSP.
    pub top: u64,
}

struct Manager {
    window: PhysWindow,
    root: PhysFrame,
    frames: FrameBitmap<&'static mut [u64]>,
    slot_base: u64,
    next_stack: u64,
    next_scratch: u64,
}

static MANAGER: TryLock<Option<Manager>> = TryLock::new(None);

/// Frame source for new page tables.
struct Tables<'a>(&'a mut FrameBitmap<&'static mut [u64]>);

impl FrameSource for Tables<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        self.0.allocate().ok()
    }
}

impl Manager {
    fn owns(&self, addr: u64) -> bool {
        addr >= self.slot_base && addr - self.slot_base < SLOT_SIZE
    }

    fn map_new(&mut self, page: Page, flags: Flags) -> Result<PhysFrame, MemError> {
        if !self.owns(page.start().as_u64()) {
            return Err(MemError::NotOwned);
        }
        let frame = self.frames.allocate().map_err(MemError::Frames)?;
        // SAFETY: just allocated, so the manager owns it exclusively. Zeroed
        // so no stale data leaks into the new mapping.
        unsafe { self.window.zero_frame(frame) };
        let mut mapper = Mapper::new(self.root, &mut self.window);
        match mapper.map(page, frame, flags, &mut Tables(&mut self.frames)) {
            Ok(flush) => {
                invalidate_page(flush.page().start().as_u64());
                Ok(frame)
            }
            Err(err) => {
                // Never mapped: give the frame back. Cannot fail, it was
                // allocated a moment ago.
                let _ = self.frames.free(frame);
                Err(MemError::Map(err))
            }
        }
    }

    fn unmap(&mut self, page: Page) -> Result<PhysFrame, MemError> {
        if !self.owns(page.start().as_u64()) {
            return Err(MemError::NotOwned);
        }
        let mut mapper = Mapper::new(self.root, &mut self.window);
        let (frame, flush) = mapper.unmap(page).map_err(MemError::Map)?;
        invalidate_page(flush.page().start().as_u64());
        Ok(frame)
    }
}

/// First virtual address covered by PML4 entry `index` (upper-half
/// addresses are sign-extended from bit 47).
fn pml4_slot_start(index: usize) -> u64 {
    let start = (index as u64) << 39;
    if index >= 256 {
        start | 0xffff_0000_0000_0000
    } else {
        start
    }
}

fn with_manager<R>(f: impl FnOnce(&mut Manager) -> R) -> Result<R, MemError> {
    let mut guard = MANAGER.try_lock().ok_or(MemError::Busy)?;
    let manager = guard.as_mut().ok_or(MemError::NotInitialized)?;
    Ok(f(manager))
}

/// One-time hand-out of the frame bitmap's static storage.
fn take_bitmap_storage() -> Option<&'static mut [u64]> {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    struct Storage(UnsafeCell<[u64; 2 * BITMAP_WORDS]>);
    // SAFETY: the storage is only reachable through the single `&mut`
    // handed out below, guarded by TAKEN.
    unsafe impl Sync for Storage {}
    static STORAGE: Storage = Storage(UnsafeCell::new([0; 2 * BITMAP_WORDS]));
    static TAKEN: AtomicBool = AtomicBool::new(false);

    if TAKEN.swap(true, Ordering::SeqCst) {
        return None;
    }
    // SAFETY: TAKEN ensures this unique mutable reference to the static is
    // created at most once.
    Some(unsafe { &mut *STORAGE.0.get() })
}

fn enable_protections(user_leaves: u64) -> Result<Protections, InitError> {
    let nx_supported = cpu::cpuid(0x8000_0001, 0).3 & bits::CPUID_EXT1_EDX_NX != 0;
    // SAFETY: EFER exists on every CPU in long mode.
    let efer = unsafe { cpu::read_msr(bits::EFER) };
    let no_execute = if efer & bits::EFER_NXE != 0 {
        Protection::On
    } else if nx_supported {
        // SAFETY: NX is supported; setting NXE makes the NX bits the
        // bootloader already put in page tables take effect.
        unsafe { cpu::write_msr(bits::EFER, efer | bits::EFER_NXE) };
        Protection::Enabled
    } else {
        return Err(InitError::NoExecuteUnsupported);
    };

    let cr0 = cpu::read_cr0();
    let write_protect = if cr0 & bits::CR0_WP != 0 {
        Protection::On
    } else {
        // SAFETY: WP makes ring 0 honour read-only pages; every page the
        // kernel writes is mapped writable.
        unsafe { cpu::write_cr0(cr0 | bits::CR0_WP) };
        Protection::Enabled
    };

    let leaf7 = cpu::cpuid(7, 0).1;
    let mut cr4 = cpu::read_cr4();
    let mut feature = |supported: bool, bit: u64| {
        if cr4 & bit != 0 {
            Protection::On
        } else if !supported {
            Protection::Unsupported
        } else if user_leaves != 0 {
            // A user-accessible kernel page would fault under SMEP/SMAP.
            Protection::Skipped
        } else {
            cr4 |= bit;
            Protection::Enabled
        }
    };
    let smep = feature(leaf7 & bits::CPUID7_EBX_SMEP != 0, bits::CR4_SMEP);
    let smap = feature(leaf7 & bits::CPUID7_EBX_SMAP != 0, bits::CR4_SMAP);
    if cr4 != cpu::read_cr4() {
        // SAFETY: only bits reported supported by CPUID are added, and no
        // user-accessible mapping exists (checked by the caller's walk), so
        // no kernel access or fetch changes behaviour.
        unsafe { cpu::write_cr4(cr4) };
    }
    Ok(Protections {
        no_execute,
        write_protect,
        smep,
        smap,
    })
}

/// Set up frame allocation, the kernel's address-space region, the heap and
/// the double-fault stack. Call once, after the CPU tables are installed.
pub fn init(boot: &BootMemory<'_>) -> Result<InitReport, InitError> {
    let offset = boot
        .physical_memory_offset
        .ok_or(InitError::NoPhysicalWindow)?;
    let highest = boot.regions.iter().map(|r| r.end).max().unwrap_or(0);
    // The bootloader maps at least [0, max(highest, 4 GiB)).
    let limit = highest.max(4 * GIB);
    if offset.checked_add(limit).is_none() {
        return Err(InitError::NoPhysicalWindow);
    }
    // SAFETY: bootloader 0.11.17 maps physical [0, align_up(max(highest,
    // 4 GiB), 2 MiB)) writable at `offset` when `mappings.physical_memory`
    // is set (see BOOTLOADER_CONFIG), and never unmaps it. Only this module
    // reaches memory through it. Checked against the page tables below.
    let mut window = unsafe { PhysWindow::new(offset, limit) };
    let root = PhysFrame::containing(
        PhysAddr::new(cpu::read_cr3() & pte::ADDRESS).ok_or(InitError::WindowMismatch)?,
    );

    // Sanity check: the window must translate to the physical addresses it
    // claims, or every later access through it would be wrong.
    let root_virt = VirtAddr::new(window.frame_virt(root)).ok_or(InitError::WindowMismatch)?;
    let translated = Mapper::new(root, &mut window).translate(root_virt);
    if translated.map(|t| t.phys) != Some(root.start()) {
        return Err(InitError::WindowMismatch);
    }

    let storage = take_bitmap_storage().ok_or(InitError::AlreadyInitialized)?;
    let usable = boot
        .regions
        .iter()
        .filter(|r| r.kind == MemoryRegionKind::Usable)
        .map(|r| (r.start, r.end));
    let (mut frames, frame_report) =
        FrameBitmap::new(storage, MIN_ALLOCATABLE, usable).map_err(InitError::FrameMap)?;

    // Withdraw everything the boot page tables use or point to.
    let window_slots = {
        let first = VirtAddr::new(offset).map(|a| a.table_index(4));
        let last = VirtAddr::new(offset + limit - 1).map(|a| a.table_index(4));
        (first.unwrap_or(0), last.unwrap_or(511))
    };
    let in_window = |index: usize| index >= window_slots.0 && index <= window_slots.1;
    let mut page_tables = 0;
    let mut boot_leaves = 0;
    let mut withdrawn = 0;
    let mut wx = 0;
    let mut user = 0;
    let mut used_slots = [false; 512];
    Mapper::new(root, &mut window).walk(in_window, |item| match item {
        WalkItem::Table { frame, .. } => {
            page_tables += 1;
            let start = frame.start().as_u64();
            withdrawn += frames.reserve(start, start + PAGE_SIZE);
        }
        WalkItem::Leaf {
            virt,
            size,
            translation,
        } => {
            boot_leaves += 1;
            used_slots[virt.table_index(4)] = true;
            let start = translation.phys.as_u64();
            withdrawn += frames.reserve(start, start.saturating_add(size));
            wx += u64::from(translation.writable && translation.executable);
            user += u64::from(translation.user);
        }
    });
    withdrawn += frames.reserve(boot.kernel_file.0, boot.kernel_file.0 + boot.kernel_file.1);

    let protections = enable_protections(user)?;

    // Claim the first upper-half PML4 slot that is unused.
    let slot = (256..512)
        .find(|&i| {
            !in_window(i) && !used_slots[i] && {
                use mm::paging::TableAccess;
                window.read(root, i) & pte::PRESENT == 0
            }
        })
        .ok_or(InitError::NoFreeSlot)?;
    let slot_base = pml4_slot_start(slot);

    {
        let mut guard = MANAGER.try_lock().ok_or(InitError::AlreadyInitialized)?;
        if guard.is_some() {
            return Err(InitError::AlreadyInitialized);
        }
        *guard = Some(Manager {
            window,
            root,
            frames,
            slot_base,
            next_stack: slot_base + STACKS_OFFSET,
            next_scratch: slot_base + SCRATCH_OFFSET,
        });
    }

    // The boot stack's guard page (left unmapped by the bootloader).
    let (stack_bottom, _) = boot.stack;
    if let Some(guard) = stack_bottom.checked_sub(PAGE_SIZE)
        && translate(guard).is_none()
    {
        register_guard(guard, stack_bottom, markers::BOOT_STACK_GUARD);
    }

    let heap_base = slot_base + HEAP_OFFSET;
    heap::init(heap_base).map_err(InitError::Heap)?;

    let df_stack =
        allocate_stack(8, markers::DOUBLE_FAULT_STACK_GUARD).map_err(InitError::Stack)?;
    // SAFETY: the stack was just mapped writable, is used by nothing else and
    // is never freed.
    unsafe { tables::set_double_fault_stack(df_stack.top) };

    Ok(InitReport {
        frames: frame_report,
        free_frames: free_frames(),
        protections,
        page_tables,
        boot_leaves,
        withdrawn_frames: withdrawn,
        writable_executable_leaves: wx,
        user_leaves: user,
        window_offset: offset,
        slot_base,
        heap_base,
        double_fault_stack: df_stack,
    })
}

/// Number of free frames (0 before init or while busy).
pub fn free_frames() -> u64 {
    with_manager(|m| m.frames.free_frames()).unwrap_or(0)
}

/// Allocate one physical frame. The caller owns it until [`free_frame`].
pub fn allocate_frame() -> Result<PhysFrame, MemError> {
    with_manager(|m| m.frames.allocate().map_err(MemError::Frames))?
}

/// Return a frame obtained from [`allocate_frame`].
pub fn free_frame(frame: PhysFrame) -> Result<(), MemError> {
    with_manager(|m| m.frames.free(frame).map_err(MemError::Frames))?
}

/// Whether `frame` is currently free in the allocator.
pub fn frame_is_free(frame: PhysFrame) -> bool {
    with_manager(|m| m.frames.is_free(frame)).unwrap_or(false)
}

/// Whether any frame overlapping physical `[start, end)` is free.
pub fn any_frame_free_in(start: u64, end: u64) -> bool {
    with_manager(|m| m.frames.any_free_in(start, end)).unwrap_or(false)
}

/// Store `value` in the first word of `frame`, through the physical window.
///
/// # Safety
///
/// The caller owns `frame` (allocated with [`allocate_frame`], not mapped or
/// used anywhere else).
pub unsafe fn write_frame_word(frame: PhysFrame, value: u64) -> Result<(), MemError> {
    // SAFETY: forwarded from the caller's contract.
    with_manager(|m| unsafe { m.window.write_u64(frame, value) })
}

/// Load the first word of `frame`, through the physical window.
///
/// # Safety
///
/// As [`write_frame_word`].
pub unsafe fn read_frame_word(frame: PhysFrame) -> Result<u64, MemError> {
    // SAFETY: forwarded from the caller's contract.
    with_manager(|m| unsafe { m.window.read_u64(frame) })
}

/// Map a fresh zeroed frame at `page` (inside the kernel's region).
pub fn map_new(page: Page, flags: Flags) -> Result<PhysFrame, MemError> {
    with_manager(|m| m.map_new(page, flags))?
}

/// Unmap `page` (inside the kernel's region) and return its frame, which the
/// caller now owns.
pub fn unmap(page: Page) -> Result<PhysFrame, MemError> {
    with_manager(|m| m.unmap(page))?
}

/// Unmap `page` and free its frame.
pub fn unmap_and_free(page: Page) -> Result<(), MemError> {
    with_manager(|m| {
        let frame = m.unmap(page)?;
        m.frames.free(frame).map_err(MemError::Frames)
    })?
}

/// Change the permissions of a mapped page inside the kernel's region.
pub fn protect(page: Page, flags: Flags) -> Result<(), MemError> {
    with_manager(|m| {
        if !m.owns(page.start().as_u64()) {
            return Err(MemError::NotOwned);
        }
        let mut mapper = Mapper::new(m.root, &mut m.window);
        let flush = mapper.protect(page, flags).map_err(MemError::Map)?;
        invalidate_page(flush.page().start().as_u64());
        Ok(())
    })?
}

/// Translate any virtual address through the active page tables.
pub fn translate(addr: u64) -> Option<Translation> {
    let addr = VirtAddr::new(addr)?;
    with_manager(|m| Mapper::new(m.root, &mut m.window).translate(addr))
        .ok()
        .flatten()
}

/// Reserve `pages` pages of scratch address space (never reused). Nothing is
/// mapped.
pub fn reserve_scratch(pages: u64) -> Result<Page, MemError> {
    with_manager(|m| {
        let start = m.next_scratch;
        let end = start
            .checked_add(pages * PAGE_SIZE)
            .filter(|&e| e <= m.slot_base + SLOT_SIZE)
            .ok_or(MemError::AddressSpaceFull)?;
        m.next_scratch = end;
        Ok(Page::containing(
            VirtAddr::new(start).ok_or(MemError::NotOwned)?,
        ))
    })?
}

/// The page starting at `addr`, if `addr` is canonical and page-aligned.
pub fn page(addr: u64) -> Option<Page> {
    Page::from_start(VirtAddr::new(addr)?)
}

/// Allocate a kernel stack of `pages` mapped pages with an unmapped guard
/// page below it. Stack address space is never reused, so the page above a
/// stack is the next stack's guard (or unmapped).
pub fn allocate_stack(pages: u64, guard_label: &'static str) -> Result<KernelStack, MemError> {
    let guard = with_manager(|m| {
        let guard = m.next_stack;
        let end = guard
            .checked_add((pages + 1) * PAGE_SIZE)
            .filter(|&e| e <= m.slot_base + SCRATCH_OFFSET)
            .ok_or(MemError::AddressSpaceFull)?;
        m.next_stack = end;
        Ok(guard)
    })??;
    let stack = KernelStack {
        guard,
        bottom: guard + PAGE_SIZE,
        top: guard + (pages + 1) * PAGE_SIZE,
    };
    for i in 0..pages {
        let page = page(stack.bottom + i * PAGE_SIZE).ok_or(MemError::NotOwned)?;
        if let Err(err) = map_new(page, Flags::DATA) {
            for j in 0..i {
                if let Some(p) = self::page(stack.bottom + j * PAGE_SIZE) {
                    let _ = unmap_and_free(p);
                }
            }
            return Err(err);
        }
    }
    register_guard(stack.guard, stack.bottom, guard_label);
    Ok(stack)
}

/// Unmap a stack from [`allocate_stack`] and free its frames.
///
/// # Safety
///
/// Nothing may use the stack any more (no CPU table points to it, no code
/// runs on it, no references into it exist).
pub unsafe fn free_stack(stack: KernelStack) -> Result<(), MemError> {
    unregister_guard(stack.guard);
    let mut addr = stack.bottom;
    while addr < stack.top {
        unmap_and_free(page(addr).ok_or(MemError::NotOwned)?)?;
        addr += PAGE_SIZE;
    }
    Ok(())
}

/// Result of [`audit_reservations`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Audit {
    pub page_tables: u64,
    pub mapped_frames: u64,
    /// Page-table or mapped frames the allocator considers free. Must be 0.
    pub free_but_in_use: u64,
    /// Leaves that are both writable and executable.
    pub writable_executable: u64,
}

/// Walk the live page tables (except the physical-memory window) and check
/// that no page-table frame and no mapped frame is free in the allocator.
pub fn audit_reservations() -> Result<Audit, MemError> {
    with_manager(|m| {
        let (offset, limit) = (m.window.offset(), m.window.limit());
        let window = |i: usize| {
            let start = pml4_slot_start(i);
            start < offset + limit && offset < start + SLOT_SIZE
        };
        let frames = &m.frames;
        let mut audit = Audit::default();
        Mapper::new(m.root, &mut m.window).walk(window, |item| match item {
            WalkItem::Table { frame, .. } => {
                audit.page_tables += 1;
                audit.free_but_in_use += u64::from(frames.is_free(frame));
            }
            WalkItem::Leaf {
                size, translation, ..
            } => {
                let start = translation.phys.as_u64();
                audit.mapped_frames += size / PAGE_SIZE;
                audit.free_but_in_use += u64::from(frames.any_free_in(start, start + size));
                audit.writable_executable +=
                    u64::from(translation.writable && translation.executable);
            }
        });
        audit
    })
}

// --- Guard registry -----------------------------------------------------------

#[derive(Clone, Copy)]
struct Guard {
    start: u64,
    end: u64,
    label: &'static str,
}

const MAX_GUARDS: usize = 16;
static GUARDS: TryLock<[Option<Guard>; MAX_GUARDS]> = TryLock::new([None; MAX_GUARDS]);

fn register_guard(start: u64, end: u64, label: &'static str) {
    if let Some(mut guards) = GUARDS.try_lock()
        && let Some(slot) = guards.iter_mut().find(|g| g.is_none())
    {
        *slot = Some(Guard { start, end, label });
    }
}

fn unregister_guard(start: u64) {
    if let Some(mut guards) = GUARDS.try_lock() {
        for slot in guards.iter_mut() {
            if slot.is_some_and(|g| g.start == start) {
                *slot = None;
            }
        }
    }
}

/// Name the guard page containing `addr`, if any. Safe to call from
/// exception handlers (never waits).
pub fn describe_guard(addr: u64) -> Option<&'static str> {
    let guards = GUARDS.try_lock()?;
    guards
        .iter()
        .flatten()
        .find(|g| addr >= g.start && addr < g.end)
        .map(|g| g.label)
}

/// Address of the first registered guard page with `label`, for tests.
pub fn find_guard(label: &str) -> Option<u64> {
    let guards = GUARDS.try_lock()?;
    guards
        .iter()
        .flatten()
        .find(|g| g.label == label)
        .map(|g| g.start)
}
