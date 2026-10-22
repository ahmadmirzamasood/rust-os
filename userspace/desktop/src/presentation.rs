//! RGB scanline presentation with a cursor separate from the retained scene.
//! Pointer-only frames examine at most two cursor-height bands, not the desktop.
use gfx::{Canvas, Layout, PixelFormat, cursor};

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    InvalidBuffer,
    Output(E),
}

#[derive(Debug)]
pub struct Presenter {
    row_layout: Layout,
    height: usize,
    previous: Option<(i32, i32)>,
    pending_scene: bool,
}

impl Presenter {
    pub fn new(width: usize, height: usize) -> Option<Self> {
        if width > 1365 || height == 0 || height > 1024 {
            return None;
        }
        Some(Self {
            row_layout: Layout::new(width, 1, width, 3, PixelFormat::Rgb).ok()?,
            height,
            previous: None,
            pending_scene: true,
        })
    }

    /// `scene` stays cursor-free. `sent` records only successfully copied rows.
    /// A failed output leaves the previous cursor position pending for retry.
    pub fn present<E>(
        &mut self,
        scene: &[u8],
        sent: &mut [u8],
        pointer: (i32, i32),
        scene_changed: bool,
        mut output: impl FnMut(usize, &[u8]) -> Result<(), E>,
    ) -> Result<usize, Error<E>> {
        let stride = self.row_layout.width() * 3;
        if scene.len() != stride * self.height || sent.len() != scene.len() {
            return Err(Error::InvalidBuffer);
        }
        self.pending_scene |= scene_changed;
        if !self.pending_scene && self.previous == Some(pointer) {
            return Ok(0);
        }
        let mut row = [0; 4096];
        let mut rows = 0;
        let full = self.pending_scene;
        for y in 0..self.height {
            let covers = |p: (i32, i32)| {
                let dy = y as i64 - i64::from(p.1);
                (0..i64::from(cursor::HEIGHT)).contains(&dy)
            };
            if !full && !covers(pointer) && !self.previous.is_some_and(covers) {
                continue;
            }
            let range = y * stride..(y + 1) * stride;
            row[..stride].copy_from_slice(&scene[range.clone()]);
            // The temporary canvas is just this row. Saturation also handles
            // hostile/off-screen pointer coordinates without integer overflow.
            if covers(pointer) {
                cursor::draw(
                    &mut Canvas::new(&mut row[..stride], self.row_layout)
                        .map_err(|_| Error::InvalidBuffer)?,
                    pointer.0,
                    pointer.1.saturating_sub(y as i32),
                );
            }
            if self.previous.is_none() || sent[range.clone()] != row[..stride] {
                output(y, &row[..stride]).map_err(Error::Output)?;
                sent[range].copy_from_slice(&row[..stride]);
                rows += 1;
            }
        }
        self.previous = Some(pointer);
        self.pending_scene = false;
        Ok(rows)
    }
}

/// A 60 Hz work budget, not an additional delay after each frame. Late frames
/// yield for a millisecond rather than accumulating deadlines or busy-spinning.
pub fn frame_sleep_ms(start_ns: u64, end_ns: u64) -> u64 {
    16_666_667u64
        .saturating_sub(end_ns.saturating_sub(start_ns))
        .div_ceil(1_000_000)
        .max(1)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn cursor_frames_match_full_composition_without_trails_or_scene_changes() {
        let (w, h) = (96, 80);
        let layout = Layout::new(w, h, w, 3, PixelFormat::Rgb).unwrap();
        let scene: std::vec::Vec<u8> = (0..w * h * 3).map(|n| (n % 251) as u8).collect();
        let mut sent = vec![0; scene.len()];
        let mut screen = sent.clone();
        let mut presenter = Presenter::new(w, h).unwrap();
        for (index, point) in [(2, 2), (60, 50), (65, 52), (95, 79), (-8, -10), (0, 0)]
            .into_iter()
            .enumerate()
        {
            let rows = presenter
                .present(&scene, &mut sent, point, false, |y, row| {
                    screen[y * w * 3..(y + 1) * w * 3].copy_from_slice(row);
                    Ok::<_, ()>(())
                })
                .unwrap();
            if index == 0 {
                assert_eq!(rows, h);
            } else {
                assert!(rows <= 2 * cursor::HEIGHT as usize);
            }
            let mut expected = scene.clone();
            cursor::draw(
                &mut Canvas::new(&mut expected, layout).unwrap(),
                point.0,
                point.1,
            );
            assert_eq!(screen, expected);
            assert_eq!(
                presenter.present(&scene, &mut sent, point, false, |_, _| Err(())),
                Ok(0)
            );
        }
        // Damage underneath a stationary cursor must still publish correctly.
        let changed = vec![23; scene.len()];
        presenter
            .present(&changed, &mut sent, (0, 0), true, |y, row| {
                screen[y * w * 3..(y + 1) * w * 3].copy_from_slice(row);
                Ok::<_, ()>(())
            })
            .unwrap();
        let mut expected = changed;
        cursor::draw(&mut Canvas::new(&mut expected, layout).unwrap(), 0, 0);
        assert_eq!(screen, expected);
    }

    #[test]
    fn rejected_buffers_and_failed_output_do_not_consume_cursor_damage() {
        let mut p = Presenter::new(64, 64).unwrap();
        let scene = vec![0; 64 * 64 * 3];
        let mut sent = scene.clone();
        assert_eq!(
            p.present(&[], &mut sent, (0, 0), false, |_, _| Ok::<_, ()>(())),
            Err(Error::InvalidBuffer)
        );
        assert_eq!(
            p.present(&scene, &mut sent, (0, 0), false, |_, _| Err(7)),
            Err(Error::Output(7))
        );
        assert_eq!(
            p.present(&scene, &mut sent, (0, 0), false, |_, _| Ok::<_, ()>(())),
            Ok(64)
        );
        let changed = vec![42; scene.len()];
        assert_eq!(
            p.present(&changed, &mut sent, (0, 0), true, |y, _| if y == 20 {
                Err(7)
            } else {
                Ok(())
            }),
            Err(Error::Output(7))
        );
        assert!(
            p.present(&changed, &mut sent, (0, 0), false, |_, _| Ok::<_, ()>(()))
                .unwrap()
                >= 44
        );
        assert_eq!(&sent[63 * 64 * 3..], &[42; 64 * 3]);
        assert_eq!(frame_sleep_ms(0, 0), 17);
        assert_eq!(frame_sleep_ms(0, 10_000_000), 7);
        assert_eq!(frame_sleep_ms(0, 40_000_000), 1);
    }
}
