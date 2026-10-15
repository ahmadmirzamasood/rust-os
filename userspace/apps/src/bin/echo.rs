//! `echo`: answers every message on its startup channel with
//! `echo: <message>` until it receives `quit`.
//!
//! Exit codes: 0 after `quit`, 1 if the other end closed, 2 on any other
//! channel error, 3 if started without a channel.

#![no_std]
#![no_main]

use ulib::{Error, limits};

ulib::entry!(main);

const PREFIX: &[u8] = b"echo: ";

fn main() -> i32 {
    let Some(channel) = ulib::startup_channel() else {
        return 3;
    };
    let mut message = [0u8; limits::MESSAGE_MAX];
    let mut reply = [0u8; limits::MESSAGE_MAX];
    loop {
        let len = match channel.recv(&mut message) {
            Ok(len) => len,
            Err(Error::PeerClosed) => return 1,
            Err(_) => return 2,
        };
        if &message[..len] == b"quit" {
            return 0;
        }
        let body = len.min(reply.len() - PREFIX.len());
        reply[..PREFIX.len()].copy_from_slice(PREFIX);
        reply[PREFIX.len()..PREFIX.len() + body].copy_from_slice(&message[..body]);
        if channel.send(&reply[..PREFIX.len() + body]).is_err() {
            return 2;
        }
    }
}
