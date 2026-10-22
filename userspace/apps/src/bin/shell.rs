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
use desktop::presentation::{Presenter, frame_sleep_ms};
use desktop::{
    display::{Action, Display, Event},
    shell::{APPS, PREF_PATH, Preferences, Shell, TASK_MANAGER},
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
fn paint(shell: &mut Shell, display: &mut Display, app: usize, browsers: &[Browser; 2]) {
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
    if app >= APPS.len() {
        let action = (app - APPS.len() + 1) as u64;
        shell.request_power(
            action,
            browsers.iter().any(Browser::captures_escape) || shell.task_confirm.is_some(),
        );
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
fn workspace(shell: &mut Shell, display: &mut Display, index: usize, move_focused: bool) {
    let moving = display.focus().filter(|_| move_focused);
    let result = (|| {
        if let Some(id) = moving {
            display.move_to_workspace(id, index)?;
        }
        display.switch_workspace(index)?;
        if let Some(id) = moving {
            display.raise(id)?;
        }
        Ok::<(), desktop::display::Error>(())
    })();
    if result.is_ok() {
        shell.menu = false;
        display.cancel_capture();
        ulib::println!("SHELL WORKSPACE {} moved={}", index, moving.is_some());
    }
}
fn run() -> ulib::Result<()> {
    let (width, height) = ulib::display::dimensions()?;
    let mut invalid_snapshot = [0x5a; os_abi::monitor::SIZE - 1];
    if ulib::display::system_snapshot_into(&mut invalid_snapshot) != Err(Error::InvalidArgument)
        || invalid_snapshot.iter().any(|b| *b != 0x5a)
    {
        return Err(Error::Io);
    }
    ulib::println!("SHELL MONITOR VALIDATION PASS");
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
    let mut presenter = Presenter::new(width, height).ok_or(Error::InvalidArgument)?;
    let mut display = Display::new(width as u32, height as u32).map_err(|_| Error::NoResources)?;
    display.set_task_strip(false);
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
    let mut drag: Option<(u32, i32, i32)> = None;
    let mut resize: Option<(u32, i32, i32, i32, i32)> = None;
    let mut last_second = u64::MAX;
    let mut first_frame = true;
    let mut power_wait: Option<(u64, u64)> = None;
    let mut stats_start = ulib::clock_ns();
    let (mut frames, mut scenes, mut rows, mut work_ns) = (0u64, 0u64, 0u64, 0u64);
    let (mut cursor_frames, mut cursor_rows) = (0u64, 0u64);
    let (mut input_epoch, mut presented_input) = (0u64, 0u64);
    loop {
        let frame_start = ulib::clock_ns();
        for _ in 0..64 {
            let p = match ulib::display::input() {
                Ok(p) => p,
                Err(Error::WouldBlock) => break,
                Err(e) => return Err(e),
            };
            let x = i32::from_le_bytes(p[4..8].try_into().unwrap());
            let y = i32::from_le_bytes(p[8..12].try_into().unwrap());
            shell.sync_controls(&mut display);
            // Pointer-only motion is composed by Presenter over the retained
            // scene; dragging and hover changes invalidate their own damage.
            if p[0] != 2 {
                input_epoch = input_epoch.saturating_add(1);
                display.invalidate();
            }
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
                    0x01 if !shell.menu
                        && display
                            .focus()
                            .and_then(|id| shell.app_for(id))
                            .is_some_and(|app| {
                                ((app == 1 || app == 2) && browsers[app - 1].captures_escape())
                                    || (app == TASK_MANAGER && shell.task_confirm.is_some())
                            }) =>
                    {
                        event = display.key(x as u32, true);
                    }
                    0x01 => {
                        let before = shell.launcher.query().len();
                        shell.dismiss_menu();
                        if before != shell.launcher.query().len() {
                            ulib::println!(
                                "SHELL SEARCH length={} results={}",
                                shell.launcher.query().len(),
                                shell.launcher.result_count()
                            );
                        }
                    }
                    0xe05b | 0xe05c => {
                        shell.toggle_menu();
                    }
                    _ if shell.menu => {
                        let before = shell.launcher.query().len();
                        let app = shell.menu_key(x as u32);
                        if before != shell.launcher.query().len() {
                            ulib::println!(
                                "SHELL SEARCH length={} results={}",
                                shell.launcher.query().len(),
                                shell.launcher.result_count()
                            );
                        }
                        if let Some(app) = app {
                            launch(&mut shell, &mut display, app, &mut browsers);
                        }
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
                        for id in shell.windows.iter().flatten().copied() {
                            if display.workspace_of(id) == Some(display.current_workspace()) {
                                let _ = display.action(id, Action::Restore);
                                let _ = display.raise(id);
                            }
                        }
                        ulib::println!("SHELL RESTORE");
                    }
                    0x3d => {
                        if let Some(id) = display.focus() {
                            let _ = shell.close(&mut display, id);
                            ulib::println!("SHELL CLOSE {}", id);
                        }
                    }
                    0x3e => {
                        shell.reduced_motion = !shell.reduced_motion;
                        paint(&mut shell, &mut display, 3, &browsers);
                        ulib::println!("SHELL REDUCED MOTION {}", shell.reduced_motion);
                    }
                    0x3f => {
                        launch(&mut shell, &mut display, TASK_MANAGER, &mut browsers);
                    }
                    0x40..=0x42 => {
                        if let Some(id) = display.focus() {
                            let area = Shell::work_area(width as u32, height as u32);
                            let result = if x == 0x40 {
                                display.toggle_maximize(id, area)
                            } else {
                                let half = (area.w - 8) / 2;
                                display.place(
                                    id,
                                    gfx::Rect::new(
                                        if x == 0x41 {
                                            area.x
                                        } else {
                                            area.x + half as i32 + 8
                                        },
                                        area.y,
                                        half,
                                        area.h,
                                    ),
                                )
                            };
                            if result.is_ok() {
                                ulib::println!("SHELL LAYOUT {} {}", id, x);
                            }
                        }
                    }
                    0x43 => {
                        for (app, id) in shell
                            .windows
                            .iter()
                            .enumerate()
                            .filter_map(|(a, w)| w.map(|w| (a, w)))
                        {
                            if display.workspace_of(id) != Some(display.current_workspace()) {
                                continue;
                            }
                            let _ = display.action(id, Action::Restore);
                            let _ = display.action(id, Action::Resize(480, 236));
                            let _ = display.action(
                                id,
                                Action::Move(160 + app as i32 * 38, 100 + app as i32 * 38),
                            );
                        }
                        ulib::println!("SHELL CASCADE");
                    }
                    0x44 | 0x57 | 0x58 => {
                        let current = display.current_workspace();
                        let count = desktop::display::WORKSPACE_COUNT;
                        let index = (current + if x == 0x44 { count - 1 } else { 1 }) % count;
                        workspace(&mut shell, &mut display, index, x == 0x58);
                        drag = None;
                        resize = None;
                    }
                    _ if !shell.menu => {
                        event = display.key(x as u32, true);
                    }
                    _ => {}
                },
                1 if p[1] == 0 && !shell.menu => {
                    event = display
                        .key(x as u32, false)
                        .filter(|d| shell.app_for(d.window) == Some(3));
                }
                2 => {
                    pointer = (x, y);
                    // Launching/focusing an existing app can also switch the
                    // workspace. Never keep a runtime drag on a hidden window.
                    if drag.is_some_and(|(id, _, _)| {
                        shell.menu || display.workspace_of(id) != Some(display.current_workspace())
                    }) {
                        drag = None;
                    }
                    if resize.is_some_and(|(id, _, _, _, _)| {
                        shell.menu || display.workspace_of(id) != Some(display.current_workspace())
                    }) {
                        resize = None;
                    }
                    if drag.is_none() && resize.is_none() {
                        shell.controls_motion(&mut display, x, y);
                    }
                    if shell.pointer_motion(
                        x,
                        y,
                        display.motion(x, y).is_some(),
                        width as u32,
                        height as u32,
                    ) {
                        display.invalidate();
                    }
                    if let Some((id, sx, sy, w, h)) = resize {
                        let new_w = (w + x - sx).clamp(256, 1024) as u32;
                        let new_h = (h + y - sy).clamp(160, 512) as u32;
                        let _ = display.action(id, Action::Resize(new_w, new_h));
                    }
                    if let Some((id, dx, dy)) = drag {
                        let _ = display.action(
                            id,
                            Action::Move(x - dx, (y - dy).clamp(44, (height as i32 - 120).max(44))),
                        );
                    }
                }
                3 if p[2] == 0 && p[1] == 0 => {
                    pointer = (x, y);
                    if let Some((id, _, _, _, _)) = resize
                        && let Some(r) = display.geometry(id)
                    {
                        ulib::println!("SHELL RESIZE {} {} {}", id, r.w, r.h);
                    }
                    if drag.take().is_some() | resize.take().is_some() || shell.menu {
                        display.cancel_capture();
                    } else {
                        event = display
                            .pointer(x, y, false)
                            .filter(|d| shell.app_for(d.window) == Some(3));
                    }
                }
                3 if p[2] == 0 => {
                    pointer = (x, y);
                    if (0..36).contains(&y) && (0..220).contains(&x) {
                        shell.toggle_menu();
                        continue;
                    }
                    if let Some(index) = desktop::workspaces::hit(width as u32, height as u32, x, y)
                    {
                        workspace(&mut shell, &mut display, index, false);
                        drag = None;
                        resize = None;
                        continue;
                    }
                    if shell.menu {
                        if let Some(row) = shell.menu_hit(x, y, height as u32) {
                            if let Some(app) = shell.launcher.result(row) {
                                launch(&mut shell, &mut display, app, &mut browsers);
                            }
                        } else if !(8..396).contains(&x)
                            || y < 40
                            || y > 96
                                + shell.launcher.result_count().max(1) as i32
                                    * Shell::menu_row_height(height as u32)
                                + 26
                        {
                            shell.menu = false;
                        }
                        continue;
                    }
                    if let Some(app) = desktop::dock::hit(width as u32, height as u32, x, y) {
                        if shell.windows[app].is_some() {
                            let _ = shell.dock_activate(&mut display, app);
                        } else {
                            launch(&mut shell, &mut display, app, &mut browsers);
                        }
                        ulib::println!("SHELL DOCK {}", app);
                        continue;
                    }
                    event = display.pointer(x, y, true);
                    if let Some(d) = event {
                        let r = display.geometry(d.window).ok_or(Error::NotFound)?;
                        if x >= r.x + r.w as i32 - 18 && y >= r.y + r.h as i32 - 18 {
                            resize = Some((d.window, x, y, r.w as i32, r.h as i32 - 24));
                            display.cancel_capture();
                            event = None;
                        } else if y < r.y + 24 {
                            let right = r.x + r.w as i32 - x;
                            if right <= 24 {
                                let _ = shell.close(&mut display, d.window);
                                ulib::println!("SHELL CLOSE {}", d.window);
                            } else if right <= 48 {
                                let _ = display.toggle_maximize(
                                    d.window,
                                    Shell::work_area(width as u32, height as u32),
                                );
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
                    resize = None;
                    display.cancel_capture();
                    shell.cancel_controls(&mut display);
                }
                _ => {}
            }
            if let Some(d) = event {
                if let Some(app) = shell.app_for(d.window) {
                    if app == TASK_MANAGER {
                        let code = match d.event {
                            Event::Key {
                                code,
                                pressed: true,
                            } => Some(code),
                            Event::Pointer {
                                x,
                                y,
                                pressed: true,
                            } => {
                                if shell.task_confirm.is_some() && (34..100).contains(&y) {
                                    Some(if x < 240 { 0x1c } else { 0x01 })
                                } else if (34..56).contains(&y) {
                                    Some(0x19)
                                } else if !shell.process_view && (56..188).contains(&y) {
                                    if let Some(app) = shell
                                        .windows
                                        .iter()
                                        .enumerate()
                                        .nth((y as usize - 56) / 22)
                                        .map(|(a, _)| a)
                                    {
                                        shell.task_selected = app;
                                    }
                                    None
                                } else if !shell.process_view && y >= 188 {
                                    Some(if x < 150 {
                                        0x1c
                                    } else if x < 300 {
                                        0x32
                                    } else {
                                        0xe053
                                    })
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        if let Some(code) = code {
                            let _ = shell.task_key(&mut display, code);
                        }
                        paint(&mut shell, &mut display, TASK_MANAGER, &browsers);
                        ulib::println!(
                            "SHELL TASKS selected={} processes={} confirm={}",
                            shell.task_selected,
                            shell.process_view,
                            shell.task_confirm.is_some()
                        );
                    }
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
                        paint(&mut shell, &mut display, app, &browsers);
                    }
                }
                let motion_before = shell.reduced_motion;
                let save_preferences = shell.activate(&mut display, d.window, d.event);
                if motion_before != shell.reduced_motion {
                    ulib::println!("SHELL ICED MOTION {}", shell.reduced_motion);
                }
                if save_preferences {
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
                    paint(&mut shell, &mut display, 3, &browsers);
                }
                if let Event::Key { .. } = d.event {
                    ulib::println!("SHELL KEY window {}", d.window);
                }
            }
        }
        shell.sync_controls(&mut display);
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
                    paint(&mut shell, &mut display, i + 1, &browsers);
                }
                if !browsers[i].busy() {
                    let other = 1 - i;
                    if let Err(e) = browsers[other].refresh(&mut GuestFs) {
                        browsers[other].report(e);
                    }
                    paint(&mut shell, &mut display, other + 1, &browsers);
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
            match ulib::display::system_snapshot() {
                Ok(info) => {
                    if !shell.snapshot_available {
                        ulib::println!(
                            "SHELL MONITOR processes={} threads={}",
                            info.count,
                            info.threads
                        );
                    }
                    shell.snapshot = info;
                    shell.snapshot_available = true;
                }
                Err(_) => shell.snapshot_available = false,
            }
            paint(&mut shell, &mut display, TASK_MANAGER, &browsers);
            display.invalidate();
            last_second = seconds;
        }
        if shell.animate(ulib::clock_ns()) {
            display.invalidate();
        }
        let mut canvas = Canvas::new(&mut back, layout).map_err(|_| Error::InvalidArgument)?;
        let scene_changed = display
            .render_with(&mut canvas, |c| shell.background(c))
            .map_err(|_| Error::InvalidArgument)?;
        if scene_changed {
            let mut time = FmtBuf::<32>::new();
            let _ = write!(time, "Uptime {}:{:02}", seconds / 60, seconds % 60);
            shell.overlay(&mut canvas, time.as_str(), &display);
            scenes += 1;
        }
        let copied = presenter
            .present(&back, &mut sent, pointer, scene_changed, ulib::display::row)
            .map_err(|e| match e {
                desktop::presentation::Error::InvalidBuffer => Error::InvalidArgument,
                desktop::presentation::Error::Output(e) => e,
            })?;
        if copied > 0 {
            frames += 1;
            rows += copied as u64;
            if !scene_changed {
                cursor_frames += 1;
                cursor_rows += copied as u64;
            }
            if first_frame {
                ulib::println!("SHELL FRAME READY");
                first_frame = false;
            }
        }
        if scene_changed && shell.animations_settled() && input_epoch != presented_input {
            // Acknowledge key/button batches after every changed row reaches
            // the device. Pointer-only frames keep serial off their hot path.
            // Query contents stay private; only bounded UI state is reported.
            ulib::println!(
                "SHELL INPUT PRESENTED workspace={} menu={} query_len={}",
                display.current_workspace(),
                shell.menu,
                shell.launcher.query().len()
            );
            presented_input = input_epoch;
        }
        let end = ulib::clock_ns();
        work_ns += end.saturating_sub(frame_start);
        if end.saturating_sub(stats_start) >= 5_000_000_000 {
            // Actual submitted updates, including idle time; not monitor refresh
            // or a claim of vsync. Keep serial output off the per-frame path.
            ulib::println!(
                "SHELL RENDER frames={} scenes={} rows={} cursor_frames={} cursor_rows={} work_us={} interval_ms={}",
                frames,
                scenes,
                rows,
                cursor_frames,
                cursor_rows,
                work_ns / 1000,
                end.saturating_sub(stats_start) / 1_000_000
            );
            (frames, scenes, rows, work_ns) = (0, 0, 0, 0);
            (cursor_frames, cursor_rows) = (0, 0);
            stats_start = end;
        }
        ulib::sleep_ms(frame_sleep_ms(frame_start, ulib::clock_ns()));
    }
}
