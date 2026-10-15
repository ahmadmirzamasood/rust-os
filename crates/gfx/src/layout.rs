//! Validated description of a linear framebuffer.
//!
//! Firmware-reported framebuffer metadata is untrusted. [`Layout::new`]
//! rejects zero sizes, a stride narrower than the width, pixel sizes we cannot
//! encode, pixel formats that do not fit in the pixel, and any geometry whose
//! byte size overflows. Once constructed, every in-bounds offset computed by
//! [`crate::Canvas`] is guaranteed not to overflow and to lie inside a buffer
//! that passed [`Layout::check_buffer`].

use crate::theme::Rgb;

/// How a colour is stored in one pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Bytes red, green, blue (then padding).
    Rgb,
    /// Bytes blue, green, red (then padding).
    Bgr,
    /// One grayscale byte (then padding).
    Gray,
    /// 8-bit channels at the given bit offsets of a little-endian pixel.
    Bitmask {
        red_shift: u8,
        green_shift: u8,
        blue_shift: u8,
    },
}

/// Why framebuffer metadata was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// Width or height is zero.
    ZeroSize,
    /// Width, height or stride exceeds [`Layout::MAX_DIMENSION`].
    DimensionTooLarge,
    /// Stride (in pixels) is smaller than the visible width.
    StrideTooSmall { width: usize, stride: usize },
    /// Only 1 to 4 bytes per pixel are supported.
    UnsupportedBytesPerPixel(usize),
    /// The pixel format needs more bytes per pixel than reported.
    FormatDoesNotFit,
    /// Bitmask channels overlap.
    OverlappingChannels,
    /// The byte buffer is shorter than the geometry requires.
    BufferTooSmall { required: usize, actual: usize },
}

/// Framebuffer geometry and pixel encoding, validated on construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    width: usize,
    height: usize,
    stride: usize,
    bytes_per_pixel: usize,
    format: PixelFormat,
    required_len: usize,
}

impl Layout {
    /// Upper bound on width, height and stride. Far above any VM mode; it
    /// rejects garbage values early.
    pub const MAX_DIMENSION: usize = 16_384;

    /// Validate framebuffer metadata. `stride` is in pixels.
    pub fn new(
        width: usize,
        height: usize,
        stride: usize,
        bytes_per_pixel: usize,
        format: PixelFormat,
    ) -> Result<Self, LayoutError> {
        if width == 0 || height == 0 {
            return Err(LayoutError::ZeroSize);
        }
        if width > Self::MAX_DIMENSION
            || height > Self::MAX_DIMENSION
            || stride > Self::MAX_DIMENSION
        {
            return Err(LayoutError::DimensionTooLarge);
        }
        if stride < width {
            return Err(LayoutError::StrideTooSmall { width, stride });
        }
        if !(1..=4).contains(&bytes_per_pixel) {
            return Err(LayoutError::UnsupportedBytesPerPixel(bytes_per_pixel));
        }
        check_format(format, bytes_per_pixel)?;
        // The last row needs no padding after its visible pixels.
        let required_len = stride
            .checked_mul(height - 1)
            .and_then(|n| n.checked_add(width))
            .and_then(|n| n.checked_mul(bytes_per_pixel))
            .ok_or(LayoutError::DimensionTooLarge)?;
        Ok(Self {
            width,
            height,
            stride,
            bytes_per_pixel,
            format,
            required_len,
        })
    }

    pub const fn width(&self) -> usize {
        self.width
    }

    pub const fn height(&self) -> usize {
        self.height
    }

    /// Pixels from the start of one row to the start of the next.
    pub const fn stride(&self) -> usize {
        self.stride
    }

    pub const fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Minimum buffer length in bytes for this geometry.
    pub const fn required_len(&self) -> usize {
        self.required_len
    }

    /// Check that a buffer of `len` bytes can hold this geometry.
    pub const fn check_buffer(&self, len: usize) -> Result<(), LayoutError> {
        if len < self.required_len {
            Err(LayoutError::BufferTooSmall {
                required: self.required_len,
                actual: len,
            })
        } else {
            Ok(())
        }
    }

    /// Encode a colour. Only the first [`Self::bytes_per_pixel`] bytes are
    /// meaningful.
    pub const fn encode(&self, color: Rgb) -> [u8; 4] {
        let Rgb { r, g, b } = color;
        match self.format {
            PixelFormat::Rgb => [r, g, b, 0],
            PixelFormat::Bgr => [b, g, r, 0],
            PixelFormat::Gray => {
                // ITU-R BT.601 luma weights scaled to 256; the sum of the
                // weights is 256, so the result fits in a u8.
                let luma = (r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8;
                [luma as u8, 0, 0, 0]
            }
            PixelFormat::Bitmask {
                red_shift,
                green_shift,
                blue_shift,
            } => {
                // Shifts were validated to keep each channel inside 32 bits.
                let value =
                    (r as u32) << red_shift | (g as u32) << green_shift | (b as u32) << blue_shift;
                value.to_le_bytes()
            }
        }
    }
}

fn check_format(format: PixelFormat, bytes_per_pixel: usize) -> Result<(), LayoutError> {
    match format {
        PixelFormat::Rgb | PixelFormat::Bgr if bytes_per_pixel < 3 => {
            Err(LayoutError::FormatDoesNotFit)
        }
        PixelFormat::Rgb | PixelFormat::Bgr | PixelFormat::Gray => Ok(()),
        PixelFormat::Bitmask {
            red_shift,
            green_shift,
            blue_shift,
        } => {
            let bits = bytes_per_pixel * 8;
            let mut seen = 0u32;
            for shift in [red_shift, green_shift, blue_shift] {
                if usize::from(shift) + 8 > bits {
                    return Err(LayoutError::FormatDoesNotFit);
                }
                let mask = 0xffu32 << shift;
                if seen & mask != 0 {
                    return Err(LayoutError::OverlappingChannels);
                }
                seen |= mask;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_gop_mode_with_padding() {
        let layout = Layout::new(1280, 800, 1344, 4, PixelFormat::Bgr).unwrap();
        assert_eq!(layout.required_len(), (1344 * 799 + 1280) * 4);
        assert!(layout.check_buffer(1344 * 800 * 4).is_ok());
        assert_eq!(
            layout.check_buffer(100),
            Err(LayoutError::BufferTooSmall {
                required: layout.required_len(),
                actual: 100
            })
        );
    }

    #[test]
    fn rejects_invalid_geometry() {
        assert_eq!(
            Layout::new(0, 10, 10, 4, PixelFormat::Rgb),
            Err(LayoutError::ZeroSize)
        );
        assert_eq!(
            Layout::new(10, 0, 10, 4, PixelFormat::Rgb),
            Err(LayoutError::ZeroSize)
        );
        assert_eq!(
            Layout::new(640, 480, 600, 4, PixelFormat::Rgb),
            Err(LayoutError::StrideTooSmall {
                width: 640,
                stride: 600
            })
        );
        assert_eq!(
            Layout::new(640, 480, usize::MAX, 4, PixelFormat::Rgb),
            Err(LayoutError::DimensionTooLarge)
        );
        assert_eq!(
            Layout::new(640, 480, 640, 0, PixelFormat::Gray),
            Err(LayoutError::UnsupportedBytesPerPixel(0))
        );
        assert_eq!(
            Layout::new(640, 480, 640, 8, PixelFormat::Rgb),
            Err(LayoutError::UnsupportedBytesPerPixel(8))
        );
        assert_eq!(
            Layout::new(640, 480, 640, 2, PixelFormat::Bgr),
            Err(LayoutError::FormatDoesNotFit)
        );
    }

    #[test]
    fn bitmask_formats_are_checked() {
        let ok = PixelFormat::Bitmask {
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        };
        let layout = Layout::new(4, 4, 4, 4, ok).unwrap();
        assert_eq!(layout.encode(Rgb::new(1, 2, 3)), [3, 2, 1, 0]);
        let too_high = PixelFormat::Bitmask {
            red_shift: 24,
            green_shift: 8,
            blue_shift: 0,
        };
        assert_eq!(
            Layout::new(4, 4, 4, 3, too_high),
            Err(LayoutError::FormatDoesNotFit)
        );
        let overlap = PixelFormat::Bitmask {
            red_shift: 0,
            green_shift: 4,
            blue_shift: 16,
        };
        assert_eq!(
            Layout::new(4, 4, 4, 4, overlap),
            Err(LayoutError::OverlappingChannels)
        );
    }

    #[test]
    fn encodes_byte_order_per_format() {
        let c = Rgb::new(0x11, 0x22, 0x33);
        let rgb = Layout::new(1, 1, 1, 4, PixelFormat::Rgb).unwrap();
        let bgr = Layout::new(1, 1, 1, 4, PixelFormat::Bgr).unwrap();
        let gray = Layout::new(1, 1, 1, 1, PixelFormat::Gray).unwrap();
        assert_eq!(rgb.encode(c), [0x11, 0x22, 0x33, 0]);
        assert_eq!(bgr.encode(c), [0x33, 0x22, 0x11, 0]);
        assert_eq!(gray.encode(Rgb::new(255, 255, 255))[0], 255);
        assert_eq!(gray.encode(Rgb::new(0, 0, 0))[0], 0);
    }
}
