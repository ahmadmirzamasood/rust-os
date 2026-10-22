//! Bitmap-label adaptation of iced_core 0.14.0's widget::Text.
// Copyright 2019-2025 Iced contributors. Licensed under MIT; see vendor/iced/LICENSE.
// Paragraph shaping, rich text, fonts and editing are intentionally omitted.
use crate::core::layout;
use crate::core::mouse;
use crate::core::renderer;
use crate::core::widget::Tree;
use crate::core::{Color, Element, Layout, Length, Rectangle, Size, Widget};

/// The guest's fixed-font measurement and drawing boundary.
pub trait Renderer: crate::core::Renderer {
    fn measure_text(&self, content: &str) -> Size;
    fn fill_text(&mut self, content: &str, bounds: Rectangle, color: Color);
}

/// Plain borrowed text, measured and painted by the guest bitmap renderer.
#[derive(Debug, Clone, Copy)]
pub struct Text<'a> {
    content: &'a str,
    width: Length,
    height: Length,
    color: Option<Color>,
}

impl<'a> Text<'a> {
    pub fn new(content: &'a str) -> Self {
        Self {
            content,
            width: Length::Shrink,
            height: Length::Shrink,
            color: None,
        }
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
}

impl<Message, Theme, R> Widget<Message, Theme, R> for Text<'_>
where
    R: Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(&mut self, _tree: &mut Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
        layout::sized(limits, self.width, self.height, |_| {
            renderer.measure_text(self.content)
        })
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut R,
        _theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if let Some(clip) = bounds.intersection(viewport) {
            renderer.with_layer(clip, |renderer| {
                renderer.fill_text(
                    self.content,
                    bounds,
                    self.color.unwrap_or(defaults.text_color),
                );
            });
        }
    }
}

impl<'a, Message, Theme, R> From<Text<'a>> for Element<'a, Message, Theme, R>
where
    R: Renderer + 'a,
    Message: 'a,
    Theme: 'a,
{
    fn from(text: Text<'a>) -> Self {
        Element::new(text)
    }
}
