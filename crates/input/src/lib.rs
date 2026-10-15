//! Input core shared by the kernel and host tests.
//!
//! `no_std`, allocation-free and free of `unsafe` code. The kernel's
//! interrupt handlers only move raw device bytes into a [`ring::ByteRing`];
//! everything else here runs in normal (non-interrupt) context:
//!
//! - [`ring`]: a fixed-capacity single-producer/single-consumer byte ring
//!   built from atomics, with a drop counter. Full means the *new* byte is
//!   dropped and counted.
//! - [`keyboard`]: a PS/2 scancode set 1 decoder (what the i8042 delivers
//!   with translation on) producing press/release [`keyboard::KeyEvent`]s
//!   with modifiers and US-layout characters.
//! - [`mouse`]: a PS/2 three-byte mouse packet decoder that resynchronizes
//!   after lost bytes.
//! - [`pointer`]: absolute pointer position (clamped to the screen) and
//!   button state, turning packets into [`event::InputEvent`]s.
//! - [`event`]: input events and a bounded [`event::EventQueue`] that
//!   coalesces motion and otherwise drops the oldest event when full.

#![no_std]
#![forbid(unsafe_code)]

pub mod event;
pub mod keyboard;
pub mod mouse;
pub mod pointer;
pub mod ring;

#[cfg(test)]
extern crate std;
