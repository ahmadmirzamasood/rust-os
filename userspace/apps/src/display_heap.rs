//! Single-threaded display process allocator over an exclusively owned arena.
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    ptr::{self, NonNull},
};
use mm::heap::FreeListHeap;
#[derive(Debug)]
struct Heap(UnsafeCell<FreeListHeap>);
// SAFETY: this runtime has one thread per process and no user signal handlers.
// Scheduling another process switches address spaces; it cannot access this heap.
unsafe impl Sync for Heap {}
// SAFETY: only this allocator accesses the heap, and GlobalAlloc supplies live
// allocation layouts on deallocation. No allocator method calls allocating code.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the single user thread is the only accessor to this cell.
        let heap = unsafe { &mut *self.0.get() };
        heap.allocate(layout)
            .map_or(ptr::null_mut(), NonNull::as_ptr)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the single user thread is the only accessor to this cell.
        let heap = unsafe { &mut *self.0.get() };
        if let Some(ptr) = NonNull::new(ptr) {
            // SAFETY: forwarded GlobalAlloc contract: ptr is a live allocation
            // from this heap with exactly this layout, no outstanding references.
            let _ = unsafe { heap.deallocate(ptr, layout) };
        }
    }
}
#[global_allocator]
static HEAP: Heap = Heap(UnsafeCell::new(FreeListHeap::new()));
const SIZE: usize = 12 * 1024 * 1024;
#[repr(align(16))]
struct Arena([u8; SIZE]);
static mut ARENA: Arena = Arena([0; SIZE]);
pub fn init() {
    // SAFETY: called once at process startup, before any allocation; the private
    // static arena is writable, aligned and never otherwise accessed or aliased.
    let arena = unsafe { core::ptr::addr_of_mut!(ARENA.0).cast() };
    // SAFETY: the single process thread is the only accessor, before allocations.
    let heap = unsafe { &mut *HEAP.0.get() };
    // SAFETY: arena is the exclusively owned, writable static span described above.
    unsafe {
        heap.init(arena, SIZE);
    }
}
