//! Kernel panic handler.
//!
//! Reports over serial first (the path most likely to work), then tries to
//! draw a banner on screen, then halts. It never allocates and never waits on
//! a lock: the framebuffer is skipped if the panic interrupted drawing.

use core::fmt::Write;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use boot_data::markers;
use gfx::FmtBuf;

use crate::{arch, display, kprintln, serial};

static PANICKING: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    arch::disable_interrupts();
    if PANICKING.swap(true, Ordering::SeqCst) {
        // Panicked while reporting a panic: say so as simply as possible.
        kprintln!("{} (nested panic while reporting; halting)", markers::PANIC);
        arch::halt_forever();
    }
    if !serial::is_ready() {
        serial::init();
    }

    kprintln!("{}", markers::PANIC);
    match info.location() {
        Some(loc) => kprintln!(
            "{} {}:{}:{}",
            markers::PANIC_LOCATION,
            loc.file(),
            loc.line(),
            loc.column()
        ),
        None => kprintln!("{} unknown", markers::PANIC_LOCATION),
    }
    kprintln!("[rust-os]   message: {}", info.message());

    let mut message = FmtBuf::<160>::new();
    let _ = write!(message, "{}", info.message());
    let mut location = FmtBuf::<160>::new();
    if let Some(loc) = info.location() {
        let _ = write!(
            location,
            "at {}:{}:{}",
            loc.file(),
            loc.line(),
            loc.column()
        );
    }
    let drawn = display::with_canvas(|canvas| {
        gfx::screens::draw_panic_banner(canvas, &[message.as_str(), location.as_str()]);
    })
    .is_some();
    if drawn {
        kprintln!("{}", markers::PANIC_BANNER_DRAWN);
    } else {
        kprintln!("[rust-os]   on-screen panic banner: not drawn (no usable framebuffer)");
    }
    kprintln!("{}", markers::PANIC_HALTED);
    arch::halt_forever()
}
