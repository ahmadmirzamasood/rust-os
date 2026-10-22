# iced_widget guest subset

This directory ports source from `iced_widget` 0.14.0 and the plain-text
widget in `iced_core` 0.14.0. It is a deliberately limited fork, not the full
upstream API. The baseline is the source in the published crate archives.

- `Button`, `Row`, `Column`, and `Space` retain upstream layout, retained
  state-tree reconciliation and mouse-message behavior. Layout uses the
  ported upstream flex solver in `iced_core`.
- Imports use `core`/`alloc`. There are no registry or native dependencies,
  unsafe blocks, build scripts, host windows or standard-library calls.
- Generic theme and renderer parameters are explicit. Button styling uses
  a supplied closure instead of the upstream palette/catalog. Supported
  backgrounds are solid colors.
- `Text` adapts upstream's measured text/layout/drawing boundary to a fixed
  bitmap-label renderer. Rich text, shaping, fonts and paragraph editing
  are omitted. It clips rendering to its layout and viewport.
- Overlay, widget operations, wrapping rows/columns, clipboard and touch
  support are omitted. Mouse `CursorLeft` cancels a pending button press,
  allowing guest focus/input-loss notifications to release capture.
- A button activates on release inside its bounds and viewport after an
  inside press. Disabled buttons do not publish messages. Styling is derived
  from the retained state tree on draw, so rebuilding a view preserves a
  pending press.

The Iced MIT notice is in `../LICENSE`. The core flex solver separately
preserves its Apache-2.0 notice and license. New port-specific adaptations
are distributed under this repository's MIT license.
