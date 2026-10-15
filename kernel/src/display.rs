//! Ownership of the firmware framebuffer.
//!
//! [`install`] validates the bootloader's framebuffer description and moves
//! the framebuffer into a kernel-global slot. Drawing goes through
//! [`with_canvas`], which hands out a bounds-checked [`gfx::Canvas`].

use bootloader_api::info::{FrameBuffer, PixelFormat as BootPixelFormat};
use gfx::{Canvas, Layout, LayoutError, PixelFormat};

use crate::sync::TryLock;

/// Why the framebuffer cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayError {
    /// The bootloader reported a pixel format this kernel does not know.
    UnknownPixelFormat,
    /// The geometry or buffer length is invalid.
    Layout(LayoutError),
    /// The buffer address range wraps around the address space.
    BadAddress,
    /// A framebuffer was already installed.
    AlreadyInstalled,
}

#[derive(Debug)]
struct Display {
    buffer: &'static mut [u8],
    layout: Layout,
}

static DISPLAY: TryLock<Option<Display>> = TryLock::new(None);

/// Validate `framebuffer` and keep it for drawing. On error the framebuffer is
/// dropped unused and the kernel continues with serial output only.
pub fn install(framebuffer: FrameBuffer) -> Result<Layout, DisplayError> {
    let info = framebuffer.info();
    let format = match info.pixel_format {
        BootPixelFormat::Rgb => PixelFormat::Rgb,
        BootPixelFormat::Bgr => PixelFormat::Bgr,
        BootPixelFormat::U8 => PixelFormat::Gray,
        BootPixelFormat::Unknown {
            red_position,
            green_position,
            blue_position,
        } => PixelFormat::Bitmask {
            red_shift: red_position,
            green_shift: green_position,
            blue_shift: blue_position,
        },
        _ => return Err(DisplayError::UnknownPixelFormat),
    };
    let layout = Layout::new(
        info.width,
        info.height,
        info.stride,
        info.bytes_per_pixel,
        format,
    )
    .map_err(DisplayError::Layout)?;
    layout
        .check_buffer(info.byte_len)
        .map_err(DisplayError::Layout)?;

    // `into_buffer` builds a `&'static mut [u8]` from the bootloader's
    // virtual address and `byte_len`. We rely on bootloader 0.11.17 having
    // mapped exactly that range writable for the kernel and handing it over
    // exactly once (we `take()` it out of BootInfo, so nothing else aliases
    // it). We can at least reject an address range that wraps.
    let buffer = framebuffer.into_buffer();
    if (buffer.as_ptr() as usize)
        .checked_add(buffer.len())
        .is_none()
    {
        return Err(DisplayError::BadAddress);
    }

    let mut slot = DISPLAY.try_lock().ok_or(DisplayError::AlreadyInstalled)?;
    if slot.is_some() {
        return Err(DisplayError::AlreadyInstalled);
    }
    *slot = Some(Display { buffer, layout });
    Ok(layout)
}

/// Run `draw` on the framebuffer. Returns `None` if no framebuffer is
/// installed or it is busy (only possible when a panic interrupts drawing).
pub fn with_canvas<R>(draw: impl FnOnce(&mut Canvas<'_>) -> R) -> Option<R> {
    let mut slot = DISPLAY.try_lock()?;
    let display = slot.as_mut()?;
    let mut canvas = Canvas::new(display.buffer, display.layout).ok()?;
    Some(draw(&mut canvas))
}
