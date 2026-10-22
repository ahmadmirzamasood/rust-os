//! A guest `no_std` port of the Iced 0.14 core widget and layout foundations.
//! See `../../PORT.md` for the upstream revision, supported subset and changes.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod alignment;
mod background;
pub mod border;
mod color;
mod element;
pub mod event;
pub mod layout;
mod length;
pub mod mouse;
pub mod padding;
mod pixels;
mod point;
mod rectangle;
pub mod renderer;
mod shadow;
mod shell;
mod size;
mod vector;
pub mod widget;
pub mod window;

pub use alignment::Alignment;
pub use background::Background;
pub use border::Border;
pub use color::Color;
pub use element::Element;
pub use event::Event;
pub use layout::Layout;
pub use length::Length;
pub use padding::Padding;
pub use pixels::Pixels;
pub use point::Point;
pub use rectangle::Rectangle;
pub use renderer::Renderer;
pub use shadow::Shadow;
pub use shell::Shell;
pub use size::Size;
pub use vector::Vector;
pub use widget::Widget;

/// Rounds to the nearest unsigned coordinate, saturating out-of-range values.
/// Comparing the fractional part avoids a second rounding caused by adding 0.5
/// to a large integral `f32` or a value immediately below a half-integer.
fn round_to_u32(value: f32) -> u32 {
    let integer = value as u32;
    integer.saturating_add(u32::from(value - integer as f32 >= 0.5))
}

#[cfg(test)]
mod tests {
    use super::round_to_u32;

    #[test]
    fn unsigned_rounding_preserves_integral_floats_and_half_boundaries() {
        for (input, expected) in [
            (0.499_999_97, 0),
            (0.5, 1),
            (1.5, 2),
            (-1.5, 0),
            (8_388_609.0, 8_388_609),
            (f32::INFINITY, u32::MAX),
            (f32::NEG_INFINITY, 0),
            (f32::NAN, 0),
        ] {
            assert_eq!(round_to_u32(input), expected);
        }
    }
}
