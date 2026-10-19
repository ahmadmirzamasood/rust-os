//! Ring-3 software display service: two independent channel clients; no GPU.
#![no_std]
#![no_main]
extern crate alloc;
#[path = "../display_heap.rs"]
mod heap;
use alloc::vec::Vec;
use desktop::display::{Action, Delivery, Display, Event};
use gfx::{Canvas, Layout, PixelFormat};
use ulib::{Channel, Error, Process};
ulib::entry!(main);

fn send(channel: &Channel, delivery: Delivery) {
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&delivery.window.to_le_bytes());
    match delivery.event {
        Event::Closed => bytes[4] = 3,
        Event::Motion { x, y } => {
            bytes[4] = 4;
            bytes[8..12].copy_from_slice(&x.to_le_bytes());
            bytes[12..16].copy_from_slice(&y.to_le_bytes());
        }
        Event::Key { code, pressed } => {
            bytes[4] = 1;
            bytes[5] = u8::from(pressed);
            bytes[8..12].copy_from_slice(&code.to_le_bytes());
        }
        Event::Pointer { x, y, pressed } => {
            bytes[4] = 2;
            bytes[5] = u8::from(pressed);
            bytes[8..12].copy_from_slice(&x.to_le_bytes());
            bytes[12..16].copy_from_slice(&y.to_le_bytes());
        }
    }
    let _ = channel.send(&bytes); // Nonblocking: a slow client cannot stall display.
}

fn main() -> i32 {
    heap::init();
    match run() {
        Ok(()) => 0,
        Err(e) => {
            ulib::println!("display failed: {:?}", e);
            1
        }
    }
}
fn run() -> ulib::Result<()> {
    let (width, height) = ulib::display::dimensions()?;
    if ulib::display::row(height, &[]) != Err(Error::InvalidArgument)
        || ulib::display::row(0, &[0; 3]) != Err(Error::InvalidArgument)
    {
        return Err(Error::InvalidArgument);
    }
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
    let (a, child_a) = Channel::pair()?;
    let (b, child_b) = Channel::pair()?;
    let _a = Process::spawn("colors", Some(child_a))?;
    let _b = Process::spawn("counter", Some(child_b))?;
    let channels = [a, b];
    let mut alive = [true; 2];
    let mut last_pointer = ((width as i32 - 1) / 2, (height as i32 - 1) / 2);
    let mut drag: Option<(u32, i32, i32, bool)> = None;
    let mut windows = [None; 2];

    let mut bytes = [0; 256];
    let mut redraw_pointer = true;
    ulib::println!("DISPLAY READY {}x{}; two clients started", width, height);
    loop {
        for (i, channel) in channels.iter().enumerate() {
            if !alive[i] {
                continue;
            }
            // Bound each client's work per frame to avoid starvation.
            for _ in 0..8 {
                match channel.try_recv(&mut bytes) {
                    Ok(n) => {
                        let result = display.request(i as u32 + 1, &bytes[..n]);
                        if bytes.get(1) == Some(&1)
                            && let Ok(id) = result
                        {
                            windows[i] = Some(id);
                        }
                        let mut reply = [0; 8];
                        match result {
                            Ok(id) => reply[..4].copy_from_slice(&id.to_le_bytes()),
                            Err(_) => {
                                reply[4] = 1;
                                ulib::println!("DISPLAY REJECT client {}", i + 1);
                            }
                        }
                        let _ = channel.send(&reply);
                    }
                    Err(Error::WouldBlock) => break,
                    Err(Error::PeerClosed) => {
                        display.disconnect(i as u32 + 1);
                        alive[i] = false;
                        windows[i] = None;
                        ulib::println!("DISPLAY DISCONNECT {}; service alive", i + 1);
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        for _ in 0..64 {
            let packet = match ulib::display::input() {
                Ok(p) => p,
                Err(Error::WouldBlock) => break,
                Err(e) => return Err(e),
            };
            let x = i32::from_le_bytes(packet[4..8].try_into().unwrap());
            let y = i32::from_le_bytes(packet[8..12].try_into().unwrap());
            let mut delivery = None;
            match packet[0] {
                1 => {
                    // Diagnostic handoff: stop only the display process/runtime.
                    if packet[1] != 0 && x == 0x3e {
                        return Ok(());
                    }
                    if packet[1] != 0 {
                        // F1 minimizes; F2 restores all; F3 maximizes.
                        let action = match x {
                            0x3b => Some(Action::Minimize),
                            0x3d => Some(Action::Maximize),
                            _ => None,
                        };
                        if let Some(action) = action
                            && let Some(id) = display.focus()
                        {
                            let _ = display.action(id, action);
                        }
                        if x == 0x3c {
                            for id in windows.iter().flatten() {
                                let _ = display.action(*id, Action::Restore);
                            }
                        }
                    }
                    if !matches!(x, 0x3b..=0x3d) {
                        delivery = display.key(x as u32, packet[1] != 0);
                    }
                }
                2 => {
                    last_pointer = (x, y);
                    redraw_pointer = true;
                    if drag.is_none() {
                        delivery = display.motion(x, y);
                    }
                    if let Some((id, start_x, start_y, resize)) = drag
                        && windows.contains(&Some(id))
                    {
                        let r = display.geometry(id).ok_or(Error::NotFound)?;
                        let action = if resize {
                            Action::Resize(
                                (x - r.x).clamp(64, 1024) as u32,
                                (y - r.y - 24).clamp(40, 512) as u32,
                            )
                        } else {
                            Action::Move(x - start_x, y - start_y)
                        };
                        let _ = display.action(id, action);
                    }
                }
                3 if packet[2] == 0 => {
                    last_pointer = (x, y);
                    redraw_pointer = true;
                    if packet[1] != 0
                        && let Some(id) = display.task_at(x, y)
                    {
                        let _ = display.raise(id);
                        continue;
                    }
                    delivery = display.pointer(x, y, packet[1] != 0);
                    if packet[1] == 0 {
                        drag = None;
                    } else if let Some(d) = delivery
                        && windows.contains(&Some(d.window))
                    {
                        let r = display.geometry(d.window).ok_or(Error::NotFound)?;
                        if y < r.y + 24 && r.w >= 80 && x >= r.x + r.w as i32 - 72 {
                            let right = r.x + r.w as i32 - x;
                            let action = if right <= 24 {
                                Action::Close
                            } else if right <= 48 {
                                Action::Maximize
                            } else {
                                Action::Minimize
                            };
                            let _ = display.action(d.window, action);
                            display.cancel_capture();
                            delivery = if action == Action::Close {
                                Some(Delivery {
                                    event: Event::Closed,
                                    ..d
                                })
                            } else {
                                None
                            };
                        } else if y < r.y + 24 {
                            drag = Some((d.window, x - r.x, y - r.y, false));
                        } else if x >= r.x + r.w as i32 - 12 && y >= r.y + r.h as i32 - 12 {
                            drag = Some((d.window, 0, 0, true));
                        }
                    }
                }
                4 => {
                    drag = None;
                    display.cancel_capture();
                }
                _ => {}
            }
            if let Some(d) = delivery
                && let Some(ch) = channels.get(d.client as usize - 1)
            {
                send(ch, d);
                ulib::println!("DISPLAY INPUT client {} window {}", d.client, d.window);
            }
        }
        let mut canvas = Canvas::new(&mut back, layout).map_err(|_| Error::InvalidArgument)?;
        if display
            .render(&mut canvas)
            .map_err(|_| Error::InvalidArgument)?
            || redraw_pointer
        {
            // Pointer is composed into a temporary row copy below, keeping the
            // retained scene clean when the cursor moves.
            let mut row = [0; 4096];
            for y in 0..height {
                let line = &back[y * width * 3..(y + 1) * width * 3];
                row[..line.len()].copy_from_slice(line);
                if (y as i32 - last_pointer.1).abs() < 4
                    && last_pointer.0 >= 0
                    && (last_pointer.0 as usize) < width
                {
                    let x = last_pointer.0 as usize;
                    row[x * 3..x * 3 + 3].fill(255);
                }
                let previous = &mut sent[y * width * 3..(y + 1) * width * 3];
                if previous != &row[..line.len()] {
                    ulib::display::row(y, &row[..line.len()])?;
                    previous.copy_from_slice(&row[..line.len()]);
                }
            }
            redraw_pointer = false;
        }
        ulib::sleep_ms(16);
    }
}
