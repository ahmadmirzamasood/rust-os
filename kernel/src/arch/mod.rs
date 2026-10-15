//! CPU- and platform-specific code. Everything that needs inline assembly
//! lives under this module.

pub mod x86_64;

pub use self::x86_64::{disable_interrupts, halt_forever};
