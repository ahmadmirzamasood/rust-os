//! Native adapter for the vendored iced subset. All drawing stays in ring 3.
//! The retained widget tree owns control state; the window owns its pixels.
#[cfg(test)]
#[path = "iced_tests.rs"]
mod tests;

extern crate alloc;

use alloc::vec::Vec;
use gfx::{Rect, Rgb, TextStyle};
use iced_core::{
    Background, Color, Element, Event, Length, Point, Rectangle, Size, layout, mouse, renderer,
    widget::Tree,
};
use iced_widget::{Button, Column, Text, button};

const SURFACE: Rgb = Rgb::new(238, 242, 246);
const TEXT: Color = Color::from_rgb(0.13, 0.17, 0.23);

/// Borrowed software renderer. Layer clipping has a fixed depth budget; excess
/// nesting draws nothing until the matching end_layer, preserving isolation.
#[derive(Debug)]
pub struct SoftwareRenderer<'a> {
    pixels: &'a mut [Rgb],
    width: u32,
    height: u32,
    clips: [Rect; 16],
    depth: usize,
    overflow: usize,
}

impl<'a> SoftwareRenderer<'a> {
    pub fn new(pixels: &'a mut [Rgb], width: u32, height: u32) -> Option<Self> {
        if width == 0
            || height == 0
            || width > 1024
            || height > 1024
            || pixels.len() != width as usize * height as usize
        {
            return None;
        }
        Some(Self::measuring(pixels, width, height))
    }

    fn measuring(pixels: &'a mut [Rgb], width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
            clips: [Rect::new(0, 0, 0, 0); 16],
            depth: 0,
            overflow: 0,
        }
    }

    fn clip(&self) -> Rect {
        if self.overflow > 0 {
            Rect::new(0, 0, 0, 0)
        } else if self.depth == 0 {
            Rect::new(0, 0, self.width, self.height)
        } else {
            self.clips[self.depth - 1]
        }
    }

    fn bounds(rect: Rectangle) -> Option<Rect> {
        if ![rect.x, rect.y, rect.width, rect.height]
            .iter()
            .all(|v| v.is_finite())
            || rect.width <= 0.0
            || rect.height <= 0.0
        {
            return None;
        }
        Some(Rect::new(
            rect.x as i32,
            rect.y as i32,
            rect.width as u32,
            rect.height as u32,
        ))
    }

    fn intersection(a: Rect, b: Rect) -> Rect {
        let x = i64::from(a.x).max(i64::from(b.x));
        let y = i64::from(a.y).max(i64::from(b.y));
        let right = (i64::from(a.x) + i64::from(a.w)).min(i64::from(b.x) + i64::from(b.w));
        let bottom = (i64::from(a.y) + i64::from(a.h)).min(i64::from(b.y) + i64::from(b.h));
        Rect::new(
            x as i32,
            y as i32,
            (right - x).max(0) as u32,
            (bottom - y).max(0) as u32,
        )
    }

    fn color(color: Color) -> (Rgb, u8) {
        let [r, g, b, a] = color.into_rgba8();
        (Rgb::new(r, g, b), a)
    }

    fn blend(&mut self, x: i32, y: i32, color: (Rgb, u8), coverage: u8) {
        if !self.clip().contains(x, y) {
            return;
        }
        let alpha = (u32::from(color.1) * u32::from(coverage) / 255) as u8;
        // clip is always intersected with the validated surface dimensions.
        if let Some(pixel) = self
            .pixels
            .get_mut(y as usize * self.width as usize + x as usize)
        {
            *pixel = pixel.blend(color.0, alpha);
        }
    }

    fn radii(border: iced_core::border::Radius, rect: Rect) -> [u32; 4] {
        [
            border.top_left,
            border.top_right,
            border.bottom_right,
            border.bottom_left,
        ]
        .map(|v| {
            (v.max(0.0) as u32)
                .min(rect.w / 2)
                .min(rect.h / 2)
                .min(1024)
        })
    }

    fn inside(rect: Rect, radii: [u32; 4], x: i32, y: i32) -> bool {
        if !rect.contains(x, y) {
            return false;
        }
        let px = i64::from(x) - i64::from(rect.x);
        let py = i64::from(y) - i64::from(rect.y);
        let w = i64::from(rect.w);
        let h = i64::from(rect.h);
        for (r, dx, dy) in [
            (radii[0], px, py),
            (radii[1], w - 1 - px, py),
            (radii[2], w - 1 - px, h - 1 - py),
            (radii[3], px, h - 1 - py),
        ] {
            let r = i64::from(r);
            if dx < r && dy < r {
                let cx = 2 * (r - dx) - 1;
                let cy = 2 * (r - dy) - 1;
                if cx * cx + cy * cy > 4 * r * r {
                    return false;
                }
            }
        }
        true
    }
}

impl iced_core::Renderer for SoftwareRenderer<'_> {
    fn start_layer(&mut self, bounds: Rectangle) {
        if self.depth == self.clips.len() {
            self.overflow = self.overflow.saturating_add(1);
        } else {
            self.clips[self.depth] = Self::intersection(
                self.clip(),
                Self::bounds(bounds).unwrap_or(Rect::new(0, 0, 0, 0)),
            );
            self.depth += 1;
        }
    }

    fn end_layer(&mut self) {
        if self.overflow > 0 {
            self.overflow -= 1;
        } else {
            self.depth = self.depth.saturating_sub(1);
        }
    }

    fn fill_quad(&mut self, quad: renderer::Quad, background: impl Into<Background>) {
        let Some(rect) = Self::bounds(quad.bounds) else {
            return;
        };
        let Background::Color(background) = background.into();
        let background = Self::color(background);
        let border_color = Self::color(quad.border.color);
        let radii = Self::radii(quad.border.radius, rect);
        let border = (quad.border.width.max(0.0) as u32)
            .min(rect.w / 2)
            .min(rect.h / 2);
        let inner = Rect::new(
            rect.x.saturating_add(border as i32),
            rect.y.saturating_add(border as i32),
            rect.w - 2 * border,
            rect.h - 2 * border,
        );
        let inner_radii = radii.map(|r| r.saturating_sub(border));
        let clip = Self::intersection(rect, self.clip());
        // Iteration is bounded by the surface, even for hostile geometry.
        for y in clip.y..clip.y + clip.h as i32 {
            for x in clip.x..clip.x + clip.w as i32 {
                if Self::inside(rect, radii, x, y) {
                    self.blend(
                        x,
                        y,
                        if border > 0 && !Self::inside(inner, inner_radii, x, y) {
                            border_color
                        } else {
                            background
                        },
                        255,
                    );
                }
            }
        }
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        self.depth = 0;
        self.overflow = 0;
        self.start_layer(new_bounds);
    }
}

impl iced_widget::text::Renderer for SoftwareRenderer<'_> {
    fn measure_text(&self, content: &str) -> Size {
        Size::new(
            TextStyle::Body.measure(content) as f32,
            TextStyle::Body.line_height() as f32,
        )
    }

    fn fill_text(&mut self, content: &str, bounds: Rectangle, color: Color) {
        use iced_core::Renderer;
        self.start_layer(bounds);
        let color = Self::color(color);
        let mut pen = bounds.x as i32;
        for ch in content.chars() {
            if pen >= self.width as i32 {
                break;
            }
            if let Some(glyph) = TextStyle::Body.glyph(ch) {
                for (dy, row) in glyph.raster().iter().enumerate() {
                    for (dx, coverage) in row.iter().enumerate() {
                        self.blend(
                            pen.saturating_add(dx as i32),
                            (bounds.y as i32).saturating_add(dy as i32),
                            color,
                            *coverage,
                        );
                    }
                }
            }
            pen = pen.saturating_add(TextStyle::Body.char_width() as i32);
        }
        self.end_layer();
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Model<'a> {
    pub warm: bool,
    pub reduced_motion: bool,
    pub status: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    ToggleAccent,
    ToggleMotion,
}

#[derive(Debug, Default)]
pub struct Update {
    pub message: Option<Message>,
    pub redraw: bool,
}

/// Guest runtime: rebuild a declarative view while reconciling its state tree.
#[derive(Debug)]
pub struct Settings {
    tree: Tree,
    cursor: mouse::Cursor,
    focused: usize,
    key_down: bool,
    buttons: [Rectangle; 2],
    viewport: Rectangle,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tree: Tree::empty(),
            cursor: mouse::Cursor::Unavailable,
            focused: 0,
            key_down: false,
            buttons: [Rectangle::default(); 2],
            viewport: Rectangle::with_size(Size::new(480.0, 236.0)),
        }
    }
}

impl Settings {
    pub fn set_viewport(&mut self, width: u32, height: u32) {
        self.viewport =
            Rectangle::with_size(Size::new(width.min(480) as f32, height.min(236) as f32));
    }
    fn view<'a, 's: 'a>(
        model: Model<'a>,
        focused: usize,
    ) -> Element<'a, Message, (), SoftwareRenderer<'s>> {
        let style = move |index, status| {
            let accent = if model.warm {
                Color::from_rgb(0.61, 0.26, 0.35)
            } else {
                Color::from_rgb(0.17, 0.38, 0.65)
            };
            let background = match status {
                button::Status::Pressed => accent,
                button::Status::Hovered => Color::from_rgb(0.80, 0.86, 0.93),
                _ => Color::from_rgb(0.88, 0.91, 0.95),
            };
            button::Style {
                background: Some(Background::Color(background)),
                text_color: if status == button::Status::Pressed {
                    Color::WHITE
                } else {
                    TEXT
                },
                border: iced_core::Border::default()
                    .rounded(7)
                    .width(if index == focused { 2 } else { 1 })
                    .color(accent),
                ..button::Style::default()
            }
        };
        let accent = Button::new(Text::new(if model.warm {
            "Warm accent / change to cool"
        } else {
            "Cool accent / change to warm"
        }))
        .width(Length::Fill)
        .height(36)
        .padding([10, 12])
        .on_press(Message::ToggleAccent)
        .style(move |_: &(), status| style(0, status));
        let motion = Button::new(Text::new(if model.reduced_motion {
            "Reduced motion / enable animations"
        } else {
            "Animations enabled / reduce motion"
        }))
        .width(Length::Fill)
        .height(36)
        .padding([10, 12])
        .on_press(Message::ToggleMotion)
        .style(move |_: &(), status| style(1, status));
        Column::new()
            .padding(16)
            .spacing(10)
            .width(Length::Fill)
            .push(Text::new("Desktop appearance").color(TEXT))
            .push(accent)
            .push(motion)
            .push(Text::new(model.status).color(TEXT))
            .push(Text::new("Up/Down: select   Enter/Space: apply").color(TEXT))
            .push(Text::new("Motion applies for this boot. F4 also toggles.").color(TEXT))
            .into()
    }

    fn update_iced(&mut self, model: Model<'_>, event: Event) -> Update {
        let renderer = SoftwareRenderer::measuring(&mut [], 480, 236);
        let mut view = Self::view(model, self.focused);
        self.tree.diff(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut self.tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(480.0, 236.0)),
        );
        self.buttons = [
            layout::Layout::new(&node).child(1).bounds(),
            layout::Layout::new(&node).child(2).bounds(),
        ];
        let mut messages = Vec::new();
        let mut shell = iced_core::Shell::new(&mut messages);
        view.as_widget_mut().update(
            &mut self.tree,
            &event,
            layout::Layout::new(&node),
            self.cursor,
            &renderer,
            &mut shell,
            &self.viewport,
        );
        let redraw = shell.is_redraw_requested();
        Update {
            message: messages.pop(),
            redraw,
        }
    }

    pub fn event(&mut self, model: Model<'_>, event: crate::display::Event) -> Update {
        match event {
            crate::display::Event::Motion { x, y } => {
                if !self.viewport.contains(Point::new(x as f32, y as f32)) {
                    return self.cancel(model);
                }
                let previous = self
                    .buttons
                    .iter()
                    .position(|bounds| self.cursor.is_over(*bounds));
                self.cursor = mouse::Cursor::Available(Point::new(x as f32, y as f32));
                if self
                    .buttons
                    .iter()
                    .position(|bounds| self.cursor.is_over(*bounds))
                    == previous
                {
                    return Update::default();
                }
                self.update_iced(
                    model,
                    Event::Mouse(mouse::Event::CursorMoved {
                        position: Point::new(x as f32, y as f32),
                    }),
                )
            }
            crate::display::Event::Pointer { x, y, pressed } => {
                self.cursor = mouse::Cursor::Available(Point::new(x as f32, y as f32));
                if pressed
                    && self.cursor.is_over(self.viewport)
                    && let Some(index) = self
                        .buttons
                        .iter()
                        .position(|bounds| self.cursor.is_over(*bounds))
                {
                    self.focused = index;
                }
                self.update_iced(
                    model,
                    Event::Mouse(if pressed {
                        mouse::Event::ButtonPressed(mouse::Button::Left)
                    } else {
                        mouse::Event::ButtonReleased(mouse::Button::Left)
                    }),
                )
            }
            crate::display::Event::Key {
                code: 0xe048 | 0xe050,
                pressed: true,
            } => {
                self.cancel(model);
                self.focused ^= 1;
                Update {
                    redraw: true,
                    ..Update::default()
                }
            }
            crate::display::Event::Key {
                code: 0x1c | 0x39,
                pressed,
            } => {
                if !pressed {
                    self.key_down = false;
                    return Update::default();
                }
                if self.key_down {
                    return Update::default();
                }
                let saved = self.cursor;
                self.cancel(model);
                self.key_down = true;
                // Keyboard activation uses the same iced button state machine
                // and the bounds from the computed view, not a second hit map.
                let renderer = SoftwareRenderer::measuring(&mut [], 480, 236);
                let mut view = Self::view(model, self.focused);
                self.tree.diff(view.as_widget());
                let node = view.as_widget_mut().layout(
                    &mut self.tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(480.0, 236.0)),
                );
                let bounds = layout::Layout::new(&node).child(1 + self.focused).bounds();
                self.cursor = mouse::Cursor::Available(Point::new(
                    bounds.x + bounds.width / 2.0,
                    bounds.y + bounds.height / 2.0,
                ));
                self.update_iced(
                    model,
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                );
                let result = self.update_iced(
                    model,
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                );
                self.cursor = saved;
                result
            }
            crate::display::Event::Closed => self.cancel(model),
            _ => Update::default(),
        }
    }

    pub fn cancel(&mut self, model: Model<'_>) -> Update {
        self.key_down = false;
        if self.cursor == mouse::Cursor::Unavailable {
            return Update::default();
        }
        self.cursor = mouse::Cursor::Unavailable;
        self.update_iced(model, Event::Mouse(mouse::Event::CursorLeft))
    }

    pub fn draw(&mut self, model: Model<'_>, pixels: &mut [Rgb], width: u32, height: u32) {
        let Some(mut renderer) = SoftwareRenderer::new(pixels, width, height) else {
            return;
        };
        renderer.pixels.fill(SURFACE);
        let mut view = Self::view(model, self.focused);
        self.tree.diff(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut self.tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width as f32, height as f32)),
        );
        self.buttons = [
            layout::Layout::new(&node).child(1).bounds(),
            layout::Layout::new(&node).child(2).bounds(),
        ];
        let mut messages = Vec::new();
        view.as_widget_mut().update(
            &mut self.tree,
            &Event::Window(iced_core::window::Event::RedrawRequested),
            layout::Layout::new(&node),
            self.cursor,
            &renderer,
            &mut iced_core::Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(width as f32, height as f32)),
        );
        view.as_widget().draw(
            &self.tree,
            &mut renderer,
            &(),
            &renderer::Style { text_color: TEXT },
            layout::Layout::new(&node),
            self.cursor,
            &Rectangle::with_size(Size::new(width as f32, height as f32)),
        );
    }
}
