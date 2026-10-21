//! Shared client code linked into two separate ELF applications and processes.
use ulib::{Channel, Error};
fn transact(ch: &Channel, bytes: &[u8]) -> ulib::Result<u32> {
    ch.send(bytes)?;
    let mut reply = [0; 256];
    loop {
        let n = ch.recv(&mut reply)?;
        if n == 8 {
            return if reply[4] == 0 {
                Ok(u32::from_le_bytes(reply[..4].try_into().unwrap()))
            } else {
                Err(Error::InvalidArgument)
            };
        }
    }
}
fn fill_packet(id: u32, rgb: [u8; 3], w: u32) -> [u8; 27] {
    let mut p = [0; 27];
    p[0] = 1;
    p[1] = 2;
    p[4..8].copy_from_slice(&id.to_le_bytes());
    p[16..20].copy_from_slice(&w.to_le_bytes());
    p[20..24].copy_from_slice(&140u32.to_le_bytes());
    p[24..27].copy_from_slice(&rgb);
    p
}
fn present(ch: &Channel, id: u32) -> ulib::Result<()> {
    let mut p = [1, 3, 0, 0, 0, 0, 0, 0];
    p[4..8].copy_from_slice(&id.to_le_bytes());
    transact(ch, &p).map(drop)
}
pub fn run(title: &str, rgb: [u8; 3]) -> i32 {
    match inner(title, rgb) {
        Ok(()) => 0,
        Err(e) => {
            ulib::println!("demo error {:?}", e);
            1
        }
    }
}
fn inner(title: &str, rgb: [u8; 3]) -> ulib::Result<()> {
    if ulib::display::dimensions() != Err(Error::UnknownSyscall) {
        return Err(Error::InvalidArgument);
    }
    let ch = ulib::startup_channel().ok_or(Error::BadHandle)?;
    // An invalid client packet must fail while the channel remains usable.
    if transact(&ch, &[255; 8]) != Err(Error::InvalidArgument) {
        return Err(Error::InvalidArgument);
    }
    let mut packet = [0; 48];
    packet[0] = 1;
    packet[1] = 1;
    packet[8..12].copy_from_slice(&240u32.to_le_bytes());
    packet[12..16].copy_from_slice(&140u32.to_le_bytes());
    packet[16..16 + title.len()].copy_from_slice(title.as_bytes());
    let id = transact(&ch, &packet[..16 + title.len()])?;
    transact(&ch, &fill_packet(id, rgb, 240))?;
    let mut text = [0; 96];
    text[0] = 1;
    text[1] = 5;
    text[4..8].copy_from_slice(&id.to_le_bytes());
    text[8..12].copy_from_slice(&8u32.to_le_bytes());
    text[12..16].copy_from_slice(&8u32.to_le_bytes());
    text[16..19].fill(245);
    let label = b"Type here. F: crash";
    text[19..19 + label.len()].copy_from_slice(label);
    transact(&ch, &text[..19 + label.len()])?;
    present(&ch, id)?;
    ulib::println!("DEMO READY {} window {}", title, id);
    let mut events = 0u32;
    loop {
        let mut bytes = [0; 256];
        match ch.try_recv(&mut bytes) {
            Ok(16) => {
                if bytes[4] == 3 {
                    return Ok(());
                }
                if bytes[4] == 1 && bytes[5] == 1 {
                    ulib::println!(
                        "DEMO KEY {} {}",
                        title,
                        u32::from_le_bytes(bytes[8..12].try_into().unwrap())
                    );
                }
                events = events.wrapping_add(1);
                ulib::println!("DEMO INPUT {} event {}", title, events);
                // F deliberately panics only this client, exercising peer cleanup.
                if bytes[4] == 1 && bytes[5] == 1 && bytes[8..12] == 0x21u32.to_le_bytes() {
                    panic!("demo requested failure");
                }
                let mut changed = rgb;
                changed[0] = changed[0].wrapping_add((events as u8).wrapping_mul(7));
                let _ = ch.send(&fill_packet(id, changed, 240));
                let mut p = [1, 3, 0, 0, 0, 0, 0, 0];
                p[4..8].copy_from_slice(&id.to_le_bytes());
                let _ = ch.send(&p);
            }
            Ok(_) | Err(Error::WouldBlock) => {}
            Err(e) => return Err(e),
        }
        ulib::sleep_ms(16);
    }
}
