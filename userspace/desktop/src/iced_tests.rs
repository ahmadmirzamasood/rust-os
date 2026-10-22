use super::*;
use crate::display::Event as GuestEvent;
use alloc::vec;
use iced_core::Renderer as _;
use iced_widget::text::Renderer as _;

fn model() -> Model<'static> {
    Model {
        warm: false,
        reduced_motion: false,
        status: "Preferences saved",
    }
}

fn ready() -> (Settings, Vec<Rgb>) {
    let mut settings = Settings::default();
    let mut pixels = vec![Rgb::new(255, 0, 255); 480 * 236];
    settings.draw(model(), &mut pixels, 480, 236);
    (settings, pixels)
}

fn center(settings: &Settings, index: usize) -> (i32, i32) {
    let bounds = settings.buttons[index];
    (
        (bounds.x + bounds.width / 2.0) as i32,
        (bounds.y + bounds.height / 2.0) as i32,
    )
}

fn pointer(settings: &mut Settings, point: (i32, i32), pressed: bool) -> Update {
    settings.event(
        model(),
        GuestEvent::Pointer {
            x: point.0,
            y: point.1,
            pressed,
        },
    )
}

fn key(settings: &mut Settings, code: u32, pressed: bool) -> Update {
    settings.event(model(), GuestEvent::Key { code, pressed })
}

fn quad(bounds: Rectangle) -> renderer::Quad {
    renderer::Quad {
        bounds,
        ..renderer::Quad::default()
    }
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
    Rectangle {
        x,
        y,
        width,
        height,
    }
}

fn rgb(color: Color) -> Rgb {
    let [r, g, b, _] = color.into_rgba8();
    Rgb::new(r, g, b)
}

#[test]
fn settings_draws_controls_and_preserves_pressed_state_until_release() {
    let (mut settings, mut pixels) = ready();
    let initial = pixels.clone();
    let first = settings.buttons[0];
    let center = center(&settings, 0);
    assert_eq!(pixels[0], SURFACE);
    assert!(
        pixels[16 * 480..32 * 480]
            .iter()
            .any(|pixel| *pixel != SURFACE)
    );
    let border = first.y as usize * 480 + (first.x + first.width / 2.0) as usize;
    assert_eq!(pixels[border], rgb(Color::from_rgb(0.17, 0.38, 0.65)));
    let pressed = pointer(&mut settings, center, true);
    assert_eq!(pressed.message, None);
    assert!(pressed.redraw);
    settings.draw(model(), &mut pixels, 480, 236);
    let inside = (first.y as usize + 18) * 480 + first.x as usize + first.width as usize - 20;
    assert_ne!(pixels[inside], initial[inside]);
    assert_eq!(pixels[inside], rgb(Color::from_rgb(0.17, 0.38, 0.65)));
    assert_eq!(
        pointer(&mut settings, center, false).message,
        Some(Message::ToggleAccent)
    );
    assert_eq!(pointer(&mut settings, center, false).message, None);
}

#[test]
fn settings_cancels_capture_and_rejects_outside_or_clipped_releases() {
    let (mut settings, _) = ready();
    let first = center(&settings, 0);
    pointer(&mut settings, first, true);
    settings.cancel(model());
    assert_eq!(pointer(&mut settings, first, false).message, None);
    pointer(&mut settings, first, true);
    assert_eq!(pointer(&mut settings, (479, 235), false).message, None);
    assert_eq!(pointer(&mut settings, first, false).message, None);
    pointer(&mut settings, first, true);
    settings.set_viewport(100, 236);
    assert_eq!(pointer(&mut settings, first, false).message, None);
    settings.set_viewport(480, 236);
    assert_eq!(pointer(&mut settings, first, false).message, None);
    pointer(&mut settings, first, true);
    settings.event(model(), GuestEvent::Closed);
    assert_eq!(pointer(&mut settings, first, false).message, None);
}

#[test]
fn settings_keyboard_selection_and_repeat_use_the_same_controls() {
    let (mut settings, _) = ready();
    assert_eq!(key(&mut settings, 0xe050, true).message, None);
    assert_eq!(
        key(&mut settings, 0x1c, true).message,
        Some(Message::ToggleMotion)
    );
    assert_eq!(key(&mut settings, 0x1c, true).message, None);
    assert_eq!(key(&mut settings, 0x1c, false).message, None);
    assert_eq!(key(&mut settings, 0xe048, true).message, None);
    assert_eq!(
        key(&mut settings, 0x39, true).message,
        Some(Message::ToggleAccent)
    );
    assert_eq!(key(&mut settings, 0x39, true).message, None);
    key(&mut settings, 0x39, false);
    assert_eq!(
        key(&mut settings, 0x39, true).message,
        Some(Message::ToggleAccent)
    );
}

#[test]
fn keyboard_navigation_cancels_an_in_progress_mouse_activation() {
    let (mut settings, _) = ready();
    let accent = center(&settings, 0);
    assert_eq!(pointer(&mut settings, accent, true).message, None);
    key(&mut settings, 0xe050, true);
    assert_eq!(
        key(&mut settings, 0x1c, true).message,
        Some(Message::ToggleMotion)
    );
    assert_eq!(pointer(&mut settings, accent, false).message, None);
    key(&mut settings, 0x1c, false);
    assert_eq!(pointer(&mut settings, accent, true).message, None);
    assert_eq!(
        pointer(&mut settings, accent, false).message,
        Some(Message::ToggleAccent)
    );
}

#[test]
fn moving_within_a_hovered_button_does_not_request_another_redraw() {
    let (mut settings, _) = ready();
    let (x, y) = center(&settings, 0);
    let enter = settings.event(model(), GuestEvent::Motion { x, y });
    assert!(enter.redraw);
    assert_eq!(enter.message, None);
    let within = settings.event(model(), GuestEvent::Motion { x: x + 3, y: y + 2 });
    assert!(!within.redraw);
    assert_eq!(within.message, None);
    let second = center(&settings, 1);
    assert!(
        settings
            .event(
                model(),
                GuestEvent::Motion {
                    x: second.0,
                    y: second.1
                }
            )
            .redraw
    );
}

#[test]
fn software_layers_clip_nested_drawing_and_recover_after_depth_overflow() {
    let black = Rgb::new(0, 0, 0);
    let white = Rgb::new(255, 255, 255);
    let mut pixels = vec![black; 16 * 16];
    let mut renderer = SoftwareRenderer::new(&mut pixels, 16, 16).unwrap();
    let full = rect(0.0, 0.0, 16.0, 16.0);
    let inner = rect(2.0, 2.0, 12.0, 12.0);
    renderer.start_layer(inner);
    for _ in 1..16 {
        renderer.start_layer(full);
    }
    renderer.start_layer(full);
    renderer.start_layer(full);
    renderer.fill_quad(quad(full), Color::WHITE);
    assert!(renderer.pixels.iter().all(|pixel| *pixel == black));
    renderer.end_layer();
    renderer.fill_quad(quad(full), Color::WHITE);
    assert!(renderer.pixels.iter().all(|pixel| *pixel == black));
    renderer.end_layer();
    renderer.fill_quad(quad(full), Color::WHITE);
    for (index, pixel) in renderer.pixels.iter().enumerate() {
        let (x, y) = (index % 16, index / 16);
        assert_eq!(
            *pixel,
            if (2..14).contains(&x) && (2..14).contains(&y) {
                white
            } else {
                black
            }
        );
    }
    for _ in 0..16 {
        renderer.end_layer();
    }
    renderer.end_layer();
    renderer.fill_quad(quad(full), Color::from_rgb(1.0, 0.0, 0.0));
    assert!(
        renderer
            .pixels
            .iter()
            .all(|pixel| *pixel == Rgb::new(255, 0, 0))
    );
}

#[test]
fn software_renderer_blends_rounded_borders_and_clips_bitmap_text() {
    let black = Rgb::new(0, 0, 0);
    let mut pixels = vec![black; 24 * 24];
    let mut renderer = SoftwareRenderer::new(&mut pixels, 24, 24).unwrap();
    let mut shape = quad(rect(2.0, 2.0, 20.0, 20.0));
    shape.border = iced_core::Border::default()
        .rounded(6)
        .width(2)
        .color(Color::from_rgba(1.0, 0.0, 0.0, 0.5));
    renderer.fill_quad(shape, Color::from_rgba(0.0, 0.0, 1.0, 0.5));
    assert_eq!(renderer.pixels[2 * 24 + 2], black);
    assert_eq!(renderer.pixels[2 * 24 + 12], Rgb::new(128, 0, 0));
    assert_eq!(renderer.pixels[12 * 24 + 12], Rgb::new(0, 0, 128));
    renderer.pixels.fill(black);
    renderer.start_layer(rect(4.0, 4.0, 4.0, 10.0));
    renderer.fill_text(
        "MMMM",
        rect(0.0, 0.0, 24.0, 16.0),
        Color::from_rgba(0.0, 1.0, 0.0, 0.5),
    );
    renderer.end_layer();
    assert!(renderer.pixels.iter().any(|pixel| pixel.g > 0));
    for (index, pixel) in renderer.pixels.iter().enumerate() {
        let (x, y) = (index % 24, index / 24);
        if !(4..8).contains(&x) || !(4..14).contains(&y) {
            assert_eq!(*pixel, black);
        }
        assert_eq!(pixel.r, 0);
        assert_eq!(pixel.b, 0);
        assert!(pixel.g <= 128);
    }
}

#[test]
fn software_renderer_rejects_bad_surfaces_and_bounds_and_contains_huge_quads() {
    let sentinel = Rgb::new(11, 22, 33);
    let mut short = [sentinel; 3];
    assert!(SoftwareRenderer::new(&mut short, 2, 2).is_none());
    assert!(SoftwareRenderer::new(&mut short, 0, 3).is_none());
    assert!(SoftwareRenderer::new(&mut short, 1025, 1).is_none());
    assert_eq!(short, [sentinel; 3]);
    let mut pixels = vec![sentinel; 16 * 16];
    let mut renderer = SoftwareRenderer::new(&mut pixels, 16, 16).unwrap();
    renderer.start_layer(rect(4.0, 4.0, 4.0, 4.0));
    for bounds in [
        rect(f32::NAN, 0.0, 10.0, 10.0),
        rect(0.0, f32::INFINITY, 10.0, 10.0),
        rect(0.0, 0.0, f32::INFINITY, 10.0),
        rect(0.0, 0.0, 10.0, f32::NEG_INFINITY),
        rect(0.0, 0.0, -1.0, 10.0),
        rect(0.0, 0.0, 10.0, 0.0),
    ] {
        renderer.fill_quad(quad(bounds), Color::WHITE);
    }
    assert!(renderer.pixels.iter().all(|pixel| *pixel == sentinel));
    for bounds in [
        rect(f32::MIN, f32::MIN, f32::MAX, f32::MAX),
        rect(f32::MAX, f32::MAX, f32::MAX, f32::MAX),
        rect(-1.0e30, -1.0e30, 1.0e30, 1.0e30),
    ] {
        let mut shape = quad(bounds);
        shape.border = iced_core::Border::default()
            .rounded(f32::INFINITY)
            .width(f32::MAX)
            .color(Color::WHITE);
        renderer.fill_quad(shape, Color::WHITE);
    }
    assert!(renderer.pixels.iter().any(|pixel| *pixel != sentinel));
    for (index, pixel) in renderer.pixels.iter().enumerate() {
        let (x, y) = (index % 16, index / 16);
        if !(4..8).contains(&x) || !(4..8).contains(&y) {
            assert_eq!(*pixel, sentinel);
        }
    }
    renderer.reset(rect(f32::NAN, 0.0, 16.0, 16.0));
    let before = renderer.pixels.to_vec();
    renderer.fill_quad(quad(rect(0.0, 0.0, 16.0, 16.0)), Color::BLACK);
    assert_eq!(renderer.pixels, before);
}
