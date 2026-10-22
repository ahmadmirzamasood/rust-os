//! Draw Iced widget primitives with a guest renderer.
use crate::{Background, Border, Color, Rectangle, Shadow, Size};

/// The renderer operations used by this port's widgets.
pub trait Renderer {
    /// Starts a clipped layer. Layers must be ended in reverse order.
    fn start_layer(&mut self, bounds: Rectangle);

    /// Ends the most recently started layer.
    fn end_layer(&mut self);

    /// Draws primitives in a layer clipped to `bounds`.
    fn with_layer(&mut self, bounds: Rectangle, f: impl FnOnce(&mut Self)) {
        self.start_layer(bounds);
        f(self);
        self.end_layer();
    }

    /// Fills a quad with the provided background.
    fn fill_quad(&mut self, quad: Quad, background: impl Into<Background>);

    /// Resets the renderer to begin drawing within new bounds.
    fn reset(&mut self, new_bounds: Rectangle);
}

/// A polygon with four sides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// Bounds of the quad.
    pub bounds: Rectangle,
    /// Border, drawn inside the quad.
    pub border: Border,
    /// Shadow of the quad.
    pub shadow: Shadow,
    /// Whether bounds should be snapped to the pixel grid.
    pub snap: bool,
}

impl Default for Quad {
    fn default() -> Self {
        Self {
            bounds: Rectangle::with_size(Size::ZERO),
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// Styling attributes of a renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Text color inherited by widgets.
    pub text_color: Color,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            text_color: Color::BLACK,
        }
    }
}
