//! Window notifications supported by the guest event adapter.

/// A guest window event. The runtime supplies time separately to its frame loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A new frame is being drawn.
    RedrawRequested,
    /// Input focus or pointer capture was lost.
    Unfocused,
}

/// A request to redraw a window. Timed host requests are omitted in this port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RedrawRequest {
    /// Redraw the next frame.
    NextFrame,
    /// No redraw is needed.
    Wait,
}
