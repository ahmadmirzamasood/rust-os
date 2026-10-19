#![no_std]
#![no_main]
#[path = "../demo.rs"]
mod demo;
ulib::entry!(main);
fn main() -> i32 {
    demo::run("Input counter", [180, 100, 65])
}
