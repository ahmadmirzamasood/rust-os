//! Software rendering into a linear framebuffer.
//!
//! `no_std`, allocation-free and host-testable. The kernel hands this crate a
//! byte slice plus a [`Layout`] describing it; every drawing call is clipped
//! to the layout, honours the stride (row padding is never written) and
//! encodes colours for the reported pixel format. Nothing here touches
//! hardware or contains `unsafe` code.

#![no_std]
#![forbid(unsafe_code)]

pub mod canvas;
pub mod cursor;
pub mod layout;
pub mod screens;
pub mod text;
pub mod theme;
pub mod widgets;

pub use canvas::{Canvas, Rect};
pub use layout::{Layout, LayoutError, PixelFormat};
pub use text::{FmtBuf, TextStyle};
pub use theme::Rgb;

#[cfg(test)]
extern crate std;
