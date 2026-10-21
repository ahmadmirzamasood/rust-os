//! Ring-3 desktop shell. Built-in applications share this trusted service process.
#![no_std]
#![no_main]
extern crate alloc;
#[path = "../file_backend.rs"]
mod file_backend;
#[path = "../display_heap.rs"]
mod heap;
use alloc::vec::Vec;
use core::fmt::Write;
use desktop::files::Browser;
use desktop::{
    display::{Action, Display, Event},
    shell::{PREF_PATH, Preferences, Shell},
};
use file_backend::GuestFs;
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
fn paint(shell: &Shell, display: &mut Display, app: usize, browsers: &[Browser; 2]) {
    let text = if app == 1 || app == 2 {
        browsers[app - 1].lines().join("\n")
    } else {
        alloc::string::String::new()
    };
    if let Err(e) = shell.paint(display, app, &text) {
        ulib::println!("SHELL PAINT {:?}", e);
    }
}
fn launch(shell: &mut Shell, display: &mut Display, app: usize, browsers: &mut [Browser; 2]) {
    if app >= 5 {
        let action = (app - 4) as u64;
        shell.request_power(action, browsers.iter().any(Browser::captures_escape));
        if shell.power_confirm.is_some() {
            ulib::println!("SHELL POWER CONFIRM {}", action);
        }
        return;
    }
    match shell.launch(display, app) {
        Ok(()) => {
            if (app == 1 || app == 2)
                && let Err(e) = browsers[app - 1].refresh(&mut GuestFs)
            {
                browsers[app - 1].report(e);
            }
            paint(shell, display, app, browsers);
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
    shell.power_bits = ulib::display::power(0).unwrap_or(0);
    ulib::println!("SHELL POWER CAPABILITIES {}", shell.power_bits);
    ulib::println!(
        "SHELL READY {}x{} preference={} persistent={}",
        width,
        height,
        shell.preferences.warm,
        shell.persistent
    );
    let mut browsers = [Browser::new(false), Browser::new(true)];
    let mut pointer = (width as i32 / 2, height as i32 / 2);
    let mut drag = None;
    let mut last_second = u64::MAX;
    let mut first_frame = true;
    let mut power_wait: Option<(u64, u64)> = None;
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
            if power_wait.is_some() {
                continue;
            }
            if let Some(action) = shell.power_confirm {
                if p[0] == 1 && p[1] != 0 {
                    if x == 0x01 {
                        shell.power_confirm = None;
                        shell.power_notice = false;
                        ulib::println!("SHELL POWER CANCEL");
                    }
                    if x == 0x1c {
                        shell.power_confirm = None;
                        power_wait = Some((action, ulib::clock_ns().saturating_add(5_000_000_000)));
                        shell.status = "Stopping work; waiting for file jobs (5 seconds)";
                        ulib::println!("SHELL POWER REQUEST {}", action);
                    }
                }
                continue;
            }
            let mut event = None;
            match p[0] {
                1 if p[1] != 0 => match x {
                    0x01 if display
                        .focus()
                        .and_then(|id| shell.app_for(id))
                        .is_some_and(|app| {
                            (app == 1 || app == 2) && browsers[app - 1].captures_escape()
                        }) =>
                    {
                        event = display.key(x as u32, true);
                    }
                    0x01 => {
                        shell.menu = !shell.menu;
                        shell.selection = 0;
                    }
                    0xe048 | 0xe050 if shell.menu => {
                        shell.selection = (shell.selection + if x == 0xe048 { 7 } else { 1 }) % 8;
                    }
                    0x1c if shell.menu => {
                        let app = shell.selection;
                        launch(&mut shell, &mut display, app, &mut browsers);
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
                        if (16..310).contains(&x) && (48..320).contains(&y) {
                            launch(
                                &mut shell,
                                &mut display,
                                ((y - 48) / 34) as usize,
                                &mut browsers,
                            );
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
                        launch(
                            &mut shell,
                            &mut display,
                            ((y - 72) / 100) as usize,
                            &mut browsers,
                        );
                    }
                }
                4 => {
                    drag = None;
                    display.cancel_capture();
                }
                _ => {}
            }
            if let Some(d) = event {
                if let Some(app) = shell.app_for(d.window) {
                    if app == 0
                        && matches!(
                            d.event,
                            Event::Key {
                                code: 0x1c,
                                pressed: true
                            } | Event::Pointer { pressed: true, .. }
                        )
                    {
                        launch(&mut shell, &mut display, 1, &mut browsers);
                    }
                    if app == 1 || app == 2 {
                        let browser = &mut browsers[app - 1];
                        match d.event {
                            Event::Key {
                                code,
                                pressed: true,
                            } => browser.key(&mut GuestFs, code),
                            Event::Pointer {
                                y, pressed: true, ..
                            } => browser.click(&mut GuestFs, y),
                            _ => {}
                        }
                        paint(&shell, &mut display, app, &browsers);
                    }
                }
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
                    shell.unsaved_preferences = result.is_err();
                    ulib::println!("SHELL PREFERENCE {} {:?}", shell.preferences.warm, result);
                    paint(&shell, &mut display, 3, &browsers);
                }
                if let Event::Key { .. } = d.event {
                    ulib::println!("SHELL KEY window {}", d.window);
                }
            }
        }
        for i in 0..2 {
            let previous = browsers[i].status.clone();
            let mut changed = false;
            let io_start = ulib::clock_ns();
            // Return to input/composition after 4 ms of work or 64 steps.
            // One device request can still take the driver's bounded timeout.
            for _ in 0..64 {
                if !browsers[i].tick(&mut GuestFs) {
                    break;
                }
                changed = true;
                if ulib::clock_ns().saturating_sub(io_start) >= 4_000_000 {
                    break;
                }
            }
            if changed {
                if previous != browsers[i].status || !browsers[i].busy() {
                    ulib::println!("FILES {} {}", i, browsers[i].status);
                    paint(&shell, &mut display, i + 1, &browsers);
                }
                if !browsers[i].busy() {
                    let other = 1 - i;
                    if let Err(e) = browsers[other].refresh(&mut GuestFs) {
                        browsers[other].report(e);
                    }
                    paint(&shell, &mut display, other + 1, &browsers);
                }
            }
        }
        if let Some((action, deadline)) = power_wait {
            if ulib::clock_ns() >= deadline {
                power_wait = None;
                shell.status = "Power cancelled: application did not finish within 5 seconds";
                display.invalidate();
            } else if !browsers.iter().any(Browser::busy) {
                // Built-in views have no document editor. Dialogs were vetoed
                // before confirmation; queued operations have now acknowledged exit.
                // In-kernel services have no queued writes beyond the volume cache.
                ulib::println!("SHELL POWER APPS READY");
                let result = ulib::display::power(action);
                ulib::println!("SHELL POWER FAILED {:?}", result);
                power_wait = None;
                shell.status = "Power cancelled: flush, platform or another process failed";
                display.invalidate();
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
            if first_frame {
                ulib::println!("SHELL FRAME READY");
                first_frame = false;
            }
        }
        ulib::sleep_ms(16);
    }
}
