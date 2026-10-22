//! A guest-compatible subset port of iced_widget 0.14.0.
//! See PORT.md for the supported API and intentional omissions.
#![no_std]
#![forbid(unsafe_code)]
#![allow(clippy::too_many_arguments, clippy::type_complexity)]
extern crate alloc;
pub use iced_core as core;
pub mod button;
pub mod column;
pub mod row;
pub mod space;
pub mod text;
pub use button::Button;
pub use column::Column;
pub use row::Row;
pub use space::Space;
pub use text::Text;

#[cfg(test)]
mod tests;
