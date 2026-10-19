//! Ring-3 desktop shell. Built-in applications share this trusted service process.
#![no_std]
#![no_main]
extern crate alloc;
#[path = "../display_heap.rs"]
mod heap;
use alloc::vec::Vec;
use core::fmt::Write;
use desktop::{
    display::{Action, Display, Event},
    shell::{PREF_PATH, Preferences, Shell},
};
use gfx::{Canvas, FmtBuf, Layout, PixelFormat};
use ulib::{Error, fs};
ulib::entry!(main);
fn main() -> i32 {
    heap::init();
    match run() {
        Ok(()) => 0,
        Err(e) => {
            ulib::println!("SHELL FAILED {:?}", e);
            1
        }
    }
}
fn saved() -> Preferences {
    let mut bytes = [0; 6];
    fs::File::open(PREF_PATH, false, false)
        .ok()
        .and_then(|f| f.read(0, &mut bytes).ok())
        .and_then(|n| Preferences::decode(&bytes[..n]))
        .unwrap_or_default()
}
fn paint(shell: &Shell, display: &mut Display, app: usize) {
    let mut listing = [0; 8192];
    let text = if app == 1 {
        match fs::list("/", &mut listing) {
            Ok(0) => "(empty volume)",
            Ok(n) => core::str::from_utf8(&listing[..n]).unwrap_or("Unsupported filename encoding"),
            Err(_) => "Volume listing unavailable",
        }
    } else {
        ""
    };
    if let Err(e) = shell.paint(display, app, text) {
        ulib::println!("SHELL PAINT {:?}", e);
    }
}
fn launch(shell: &mut Shell, display: &mut Display, app: usize) {
    match shell.launch(display, app) {
        Ok(()) => {
            paint(shell, display, app);
            ulib::println!("SHELL LAUNCH {}", app);
        }
        Err(e) => {
            shell.status = "Cannot open: resources unavailable";
            ulib::println!("SHELL LAUNCH FAILED {:?}", e);
        }
    }
}
fn run() -> ulib::Result<()> {
    let (width, height) = ulib::display::dimensions()?;
    let layout = Layout::new(width, height, width, 3, PixelFormat::Rgb)
        .map_err(|_| Error::InvalidArgument)?;
    let mut back = Vec::new();
    back.try_reserve_exact(width * height * 3)
        .map_err(|_| Error::NoResources)?;
    back.resize(width * height * 3, 0);
    let mut sent = Vec::new();
    sent.try_reserve_exact(back.len())
        .map_err(|_| Error::NoResources)?;
    sent.resize(back.len(), 255);
    let mut display = Display::new(width as u32, height as u32).map_err(|_| Error::NoResources)?;
    // A mount failure must not be labelled as a temporary RAM volume.
    let mut shell = Shell::new(saved(), fs::persistent()?);
    ulib::println!(
        "SHELL READY {}x{} preference={} persistent={}",
        width,
        height,
        shell.preferences.warm,
        shell.persistent
    );
    let mut pointer = (width as i32 / 2, height as i32 / 2);
    let mut drag = None;
    let mut last_second = u64::MAX;
    loop {
        for _ in 0..64 {
            let p = match ulib::display::input() {
                Ok(p) => p,
                Err(Error::WouldBlock) => break,
                Err(e) => return Err(e),
            };
            let x = i32::from_le_bytes(p[4..8].try_into().unwrap());
            let y = i32::from_le_bytes(p[8..12].try_into().unwrap());
            display.invalidate();
            let mut event = None;
            match p[0] {
                1 if p[1] != 0 => match x {
                    0x01 => {
                        shell.menu = !shell.menu;
                        shell.selection = 0;
                    }
                    0xe048 | 0xe050 if shell.menu => {
                        shell.selection = (shell.selection + if x == 0xe048 { 4 } else { 1 }) % 5;
                    }
                    0x1c if shell.menu => {
                        let app = shell.selection;
                        launch(&mut shell, &mut display, app);
                    }
                    0x0f => {
                        let _ = shell.switch(&mut display);
                    }
                    0x3b => {
                        if let Some(id) = display.focus() {
                            let _ = display.action(id, Action::Minimize);
                            ulib::println!("SHELL MINIMIZE {}", id);
                        }
                    }
                    0x3c => {
                        for id in shell.windows.iter().flatten() {
                            let _ = display.action(*id, Action::Restore);
                            let _ = display.raise(*id);
                        }
                        ulib::println!("SHELL RESTORE");
                    }
                    0x3d => {
                        if let Some(id) = display.focus() {
                            let _ = shell.close(&mut display, id);
                            ulib::println!("SHELL CLOSE {}", id);
                        }
                    }
                    _ if !shell.menu => {
                        event = display.key(x as u32, true);
                    }
                    _ => {}
                },
                2 => {
                    pointer = (x, y);
                    if let Some((id, dx, dy)) = drag {
                        let _ = display.action(id, Action::Move(x - dx, y - dy));
                    }
                }
                3 if p[2] == 0 => {
                    pointer = (x, y);
                    if p[1] == 0 {
                        drag = None;
                        display.cancel_capture();
                        continue;
                    }
                    if y < 36 && x < 220 {
                        shell.menu = !shell.menu;
                        continue;
                    }
                    if shell.menu {
                        if (16..310).contains(&x) && (48..218).contains(&y) {
                            launch(&mut shell, &mut display, ((y - 48) / 34) as usize);
                        } else {
                            shell.menu = false;
                        }
                        continue;
                    }
                    if let Some(id) = display.task_at(x, y) {
                        let _ = display.raise(id);
                        continue;
                    }
                    event = display.pointer(x, y, true);
                    if let Some(d) = event {
                        let r = display.geometry(d.window).ok_or(Error::NotFound)?;
                        if y < r.y + 24 {
                            let right = r.x + r.w as i32 - x;
                            if right <= 24 {
                                let _ = shell.close(&mut display, d.window);
                                ulib::println!("SHELL CLOSE {}", d.window);
                            } else if right <= 48 {
                                let _ = display.action(d.window, Action::Maximize);
                            } else if right <= 72 {
                                let _ = display.action(d.window, Action::Minimize);
                            } else {
                                drag = Some((d.window, x - r.x, y - r.y));
                            }
                            display.cancel_capture();
                            event = None;
                        }
                    } else if (16..112).contains(&x) && (72..372).contains(&y) {
                        launch(&mut shell, &mut display, ((y - 72) / 100) as usize);
                    }
                }
                4 => {
                    drag = None;
                    display.cancel_capture();
                }
                _ => {}
            }
            if let Some(d) = event {
                if shell.activate(&mut display, d.window, d.event) {
                    let result = (|| {
                        let f = match fs::File::open(PREF_PATH, true, false) {
                            Ok(f) => f,
                            Err(Error::NotFound) => fs::File::open(PREF_PATH, true, true)?,
                            Err(e) => return Err(e),
                        };
                        if f.write(0, &shell.preferences.encode())? != 5 {
                            return Err(Error::Io);
                        }
                        fs::sync()
                    })();
                    shell.status = match result {
                        Ok(()) if shell.persistent => "Saved to persistent disk",
                        Ok(()) => "Applied for this boot (temporary RAM)",
                        Err(_) => "Applied; saving failed",
                    };
                    ulib::println!("SHELL PREFERENCE {} {:?}", shell.preferences.warm, result);
                    paint(&shell, &mut display, 3);
                }
                if let Event::Key { .. } = d.event {
                    ulib::println!("SHELL KEY window {}", d.window);
                }
            }
        }
        let seconds = ulib::clock_ns() / 1_000_000_000;
        if seconds != last_second {
            display.invalidate();
            last_second = seconds;
        }
        let mut canvas = Canvas::new(&mut back, layout).map_err(|_| Error::InvalidArgument)?;
        if display
            .render_with(&mut canvas, |c| shell.background(c))
            .map_err(|_| Error::InvalidArgument)?
        {
            let mut time = FmtBuf::<32>::new();
            let _ = write!(time, "Uptime {}:{:02}", seconds / 60, seconds % 60);
            shell.overlay(&mut canvas, time.as_str());
            // Every damaged scene is reconstructed before composing the cursor,
            // so moving it restores the pixels at its previous position.
            gfx::cursor::draw(&mut canvas, pointer.0, pointer.1);
            let mut row = [0; 4096];
            for y in 0..height {
                let line = &back[y * width * 3..(y + 1) * width * 3];
                row[..line.len()].copy_from_slice(line);
                let old = &mut sent[y * width * 3..(y + 1) * width * 3];
                if old != &row[..line.len()] {
                    ulib::display::row(y, &row[..line.len()])?;
                    old.copy_from_slice(&row[..line.len()]);
                }
            }
        }
        ulib::sleep_ms(16);
    }
}
