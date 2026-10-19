//! Narrow copy-only display device for the bundled display executable.
//! No client framebuffer mappings; the kernel validates every user page.
use super::{current_root, user_error, with_current};
use crate::{display, memory::user, sched};
use os_abi::{Error, syscall as nr};

pub fn call(number: u64, a: u64, b: u64, c: u64, d: u64) -> Result<u64, Error> {
    if !cfg!(feature = "desktop") || !with_current(|t, slot| Ok(t.processes[slot].display_service))?
    {
        return Err(Error::UnknownSyscall);
    }
    if d != 0 {
        return Err(Error::InvalidArgument);
    }
    let root = current_root()?;
    match number {
        nr::DISPLAY_INFO => {
            if a | b | c != 0 {
                return Err(Error::InvalidArgument);
            }
            let layout = display::layout().ok_or(Error::NotFound)?;
            if layout.width() > 1365 || layout.height() > 1024 {
                return Err(Error::TooLarge);
            }
            crate::display_device::claim(sched::current_pid());
            Ok(layout.width() as u64 | ((layout.height() as u64) << 32))
        }
        nr::DISPLAY_ROW => {
            let layout = display::layout().ok_or(Error::NotFound)?;
            if a >= layout.height() as u64 || c != (layout.width() * 3) as u64 || c > 4096 {
                return Err(Error::InvalidArgument);
            }
            let mut row = [0; 4096];
            user::copy_from(root, b, &mut row[..c as usize]).map_err(user_error)?;
            let mut pixels = [gfx::Rgb::new(0, 0, 0); 1365];
            for (pixel, rgb) in pixels.iter_mut().zip(row[..c as usize].as_chunks::<3>().0) {
                *pixel = gfx::Rgb::new(rgb[0], rgb[1], rgb[2]);
            }
            display::with_canvas(|canvas| {
                canvas.blit_pixels(0, a as i32, &pixels[..layout.width()]);
            })
            .ok_or(Error::WouldBlock)?;
            Ok(0)
        }
        nr::DISPLAY_INPUT => {
            if b != 16 || c != 0 {
                return Err(Error::InvalidArgument);
            }
            user::check(root, a, 16, true).map_err(user_error)?;
            let event = crate::display_device::pop().ok_or(Error::WouldBlock)?;
            user::copy_to(root, a, &event).map_err(user_error)?;
            Ok(16)
        }
        _ => Err(Error::UnknownSyscall),
    }
}
