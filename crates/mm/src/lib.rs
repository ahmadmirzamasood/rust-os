//! Memory-management core shared by the kernel and host tests.
//!
//! Everything here is `no_std` and independent of the running machine:
//!
//! - [`addr`]: typed physical and virtual addresses, frames and pages.
//! - [`frames`]: a bitmap physical-frame allocator that distinguishes
//!   "never allocatable" (reserved) from "allocated" frames.
//! - [`paging`]: an x86_64 4-level page-table mapper generic over how tables
//!   are reached, so the kernel drives real tables and tests drive a model.
//! - [`heap`]: a reclaiming first-fit free-list heap with coalescing.
//!
//! Only [`heap`] contains `unsafe` code (it keeps its free list inside the
//! memory it manages); the other modules forbid it.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod addr;
pub mod frames;
pub mod heap;
pub mod paging;

pub use addr::{PAGE_SIZE, Page, PhysAddr, PhysFrame, VirtAddr};
