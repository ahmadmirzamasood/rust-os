# Iced guest port

This directory contains a reduced, source-derived `no_std` port of Iced 0.14.0
for this operating system. It is not the complete upstream cross-platform
application runtime. Upstream project:
https://github.com/iced-rs/iced/tree/3997291f318a8bc06fa522f5579836fb3feb94df

The source inputs are the crates.io `iced_core` and `iced_widget` 0.14.0
archives, whose `.cargo_vcs_info.json` files identify that revision. The source
files retain their upstream algorithm and documentation where supported. The
MIT copyright and permission notice is in `LICENSE`. `core/src/layout/flex.rs`
also retains its xi-editor/Iced copyright and Apache-2.0 header; the full Apache
license is in `LICENSE-APACHE`.

## Supported foundations

The core port retains Iced's geometry, length and padding types, layout limits
and nodes, the flex layout algorithm, boxed `Element` and `Widget` interface,
persistent `widget::Tree` reconciliation, pointer events and cursor interaction,
solid sRGB color, borders, shadows and quad rendering, and `Shell` messages,
event capture and invalidation.

Changes for the guest:

- `std` imports use `core` and `alloc`; there are no registry dependencies,
  native dependencies, build scripts or unsafe blocks in these port crates.
- Widget updates omit clipboard access; operation and overlay interfaces are
  omitted. Renderer transforms and image allocation are omitted. Renderer
  clipping layers, quads and reset remain.
- Window notifications are limited to redraw and lost focus. Redraw requests
  support next frame or wait, with no host clock or asynchronous runtime.
- Mouse click counting is omitted. Keyboard, touch, IME, text editing, fonts,
  images, SVG, gradients, themes and platform-window integration are omitted.
- Geometry operations requiring trigonometry, square root, arbitrary numeric
  traits or rotation are omitted. `Point::snap` rounds nonnegative coordinates
  by comparing the fractional part against one half and performing Rust's
  saturating unsigned cast. Negative coordinates saturate to zero, as with
  upstream's unsigned result. This avoids a host math-library dependency and
  preserves large integral `f32` coordinates without double rounding.
- Color retains sRGB constructors and alpha scaling. Byte conversion rounds
  normalized nonnegative channels with the same unsigned rounding helper.
  Linear color conversion, luminance, parsing and palette helpers are
  omitted instead of approximating their math.
- Floating point layout runs through the existing soft-float guest target;
  this port does not enable CPU floating point or SIMD state.

Allocations are limited by the owning guest process's allocator. The port is
used only for trusted, project-built widget trees; it does not accept serialized
widget descriptions from other processes. The existing desktop compositor,
window policy, input and RGB scanline system calls remain the platform boundary.
