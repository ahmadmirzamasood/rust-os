use super::*;
use crate::core::{
    Background, Color, Element, Event, Layout, Length, Point, Rectangle, Shell, Size,
    layout::Limits, mouse, renderer, widget::Tree,
};
use alloc::{vec, vec::Vec};

#[derive(Default)]
struct Recorder {
    quads: Vec<Color>,
    layers: Vec<Rectangle>,
    text_bounds: Vec<Rectangle>,
}

impl crate::core::Renderer for Recorder {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push(bounds);
    }
    fn end_layer(&mut self) {}
    fn fill_quad(&mut self, _quad: renderer::Quad, background: impl Into<Background>) {
        let Background::Color(color) = background.into();
        self.quads.push(color);
    }
    fn reset(&mut self, _new_bounds: Rectangle) {}
}

impl text::Renderer for Recorder {
    fn measure_text(&self, content: &str) -> Size {
        Size::new(content.len() as f32 * 8.0, 16.0)
    }
    fn fill_text(&mut self, _content: &str, bounds: Rectangle, _color: Color) {
        self.text_bounds.push(bounds);
    }
}

type View = Element<'static, u8, (), Recorder>;

fn button(enabled: bool) -> View {
    Button::new(Text::new("Accent"))
        .width(100)
        .height(40)
        .on_press_maybe(enabled.then_some(7))
        .style(|_, status| button::Style {
            background: Some(
                Color::from_rgb(
                    if status == button::Status::Pressed {
                        1.0
                    } else {
                        0.0
                    },
                    0.0,
                    0.0,
                )
                .into(),
            ),
            ..button::Style::default()
        })
        .into()
}

fn update(
    view: &mut View,
    tree: &mut Tree,
    event: mouse::Event,
    cursor: mouse::Cursor,
    viewport: Rectangle,
) -> (Vec<u8>, bool) {
    let renderer = Recorder::default();
    let node = view.as_widget_mut().layout(
        tree,
        &renderer,
        &Limits::new(Size::ZERO, Size::new(200.0, 100.0)),
    );
    let mut messages = Vec::new();
    let mut shell = Shell::new(&mut messages);
    view.as_widget_mut().update(
        tree,
        &Event::Mouse(event),
        Layout::new(&node),
        cursor,
        &renderer,
        &mut shell,
        &viewport,
    );
    let captured = shell.is_event_captured();
    (messages, captured)
}

fn cursor(x: f32, y: f32) -> mouse::Cursor {
    mouse::Cursor::Available(Point::new(x, y))
}

fn viewport() -> Rectangle {
    Rectangle::with_size(Size::new(200.0, 100.0))
}

#[test]
fn button_activates_only_after_an_inside_press_and_inside_release() {
    let mut view = button(true);
    let mut tree = Tree::new(&view);
    let press = mouse::Event::ButtonPressed(mouse::Button::Left);
    let release = mouse::Event::ButtonReleased(mouse::Button::Left);
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![], false)
    );
    assert_eq!(
        update(&mut view, &mut tree, press, cursor(20.0, 20.0), viewport()),
        (vec![], true)
    );
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![7], true)
    );
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![], false)
    );
    update(&mut view, &mut tree, press, cursor(20.0, 20.0), viewport());
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(150.0, 20.0),
            viewport()
        ),
        (vec![], true)
    );
}

#[test]
fn disabled_hidden_and_cancelled_buttons_do_not_activate() {
    let mut view = button(false);
    let mut tree = Tree::new(&view);
    let press = mouse::Event::ButtonPressed(mouse::Button::Left);
    let release = mouse::Event::ButtonReleased(mouse::Button::Left);
    assert_eq!(
        update(&mut view, &mut tree, press, cursor(20.0, 20.0), viewport()),
        (vec![], false)
    );
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![], false)
    );
    view = button(true);
    tree.diff(&view);
    let clipped = Rectangle::new(Point::new(80.0, 0.0), Size::new(120.0, 100.0));
    assert_eq!(
        update(&mut view, &mut tree, press, cursor(20.0, 20.0), clipped),
        (vec![], false)
    );
    update(&mut view, &mut tree, press, cursor(20.0, 20.0), viewport());
    update(
        &mut view,
        &mut tree,
        mouse::Event::CursorLeft,
        mouse::Cursor::Unavailable,
        viewport(),
    );
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![], false)
    );
    update(&mut view, &mut tree, press, cursor(20.0, 20.0), viewport());
    view = button(false);
    tree.diff(&view);
    update(
        &mut view,
        &mut tree,
        release,
        cursor(20.0, 20.0),
        viewport(),
    );
    view = button(true);
    tree.diff(&view);
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            release,
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![], false)
    );
}

#[test]
fn rebuilding_the_view_preserves_a_press_in_the_tree() {
    let mut view = button(true);
    let mut tree = Tree::new(&view);
    update(
        &mut view,
        &mut tree,
        mouse::Event::ButtonPressed(mouse::Button::Left),
        cursor(20.0, 20.0),
        viewport(),
    );
    view = button(true);
    tree.diff(&view);
    let mut renderer = Recorder::default();
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &Limits::new(Size::ZERO, Size::new(200.0, 100.0)),
    );
    view.as_widget().draw(
        &tree,
        &mut renderer,
        &(),
        &renderer::Style::default(),
        Layout::new(&node),
        cursor(20.0, 20.0),
        &viewport(),
    );
    assert_eq!(renderer.quads, [Color::from_rgb(1.0, 0.0, 0.0)]);
    assert_eq!(
        update(
            &mut view,
            &mut tree,
            mouse::Event::ButtonReleased(mouse::Button::Left),
            cursor(20.0, 20.0),
            viewport()
        ),
        (vec![7], true)
    );
}

#[test]
fn rows_share_remaining_width_and_columns_apply_padding_and_spacing() {
    let row: View = Row::new()
        .spacing(10)
        .width(Length::Fill)
        .push(Space::new().width(40).height(20))
        .push(Space::new().width(Length::Fill).height(20))
        .into();
    let mut view: View = Column::new()
        .padding(10)
        .spacing(5)
        .width(Length::Fill)
        .push(row)
        .push(Space::new().height(15))
        .into();
    let mut tree = Tree::new(&view);
    let node = view.as_widget_mut().layout(
        &mut tree,
        &Recorder::default(),
        &Limits::new(Size::ZERO, Size::new(200.0, 100.0)),
    );
    let layout = Layout::new(&node);
    assert_eq!(layout.bounds().width, 200.0);
    assert_eq!(layout.child(0).bounds().x, 10.0);
    assert_eq!(layout.child(0).bounds().y, 10.0);
    assert_eq!(layout.child(0).child(1).bounds().x, 60.0);
    assert_eq!(layout.child(0).child(1).bounds().width, 130.0);
    assert_eq!(layout.child(1).bounds().y, 35.0);
}

#[test]
fn text_uses_the_measured_bounds_and_a_clipped_layer() {
    let mut view: View = Text::new("hello").into();
    let mut tree = Tree::new(&view);
    let mut renderer = Recorder::default();
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &Limits::new(Size::ZERO, Size::new(25.0, 10.0)),
    );
    let bounds = Rectangle::with_size(Size::new(25.0, 10.0));
    assert_eq!(node.bounds(), bounds);
    view.as_widget().draw(
        &tree,
        &mut renderer,
        &(),
        &renderer::Style::default(),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport(),
    );
    assert_eq!(renderer.text_bounds, [bounds]);
    assert_eq!(renderer.layers, [bounds]);
}
