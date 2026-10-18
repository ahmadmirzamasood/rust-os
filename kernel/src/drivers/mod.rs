//! Device drivers that use interrupts. (The polled COM1 diagnostics driver
//! stays in [`crate::serial`].)

pub mod ata;
pub mod ps2;
