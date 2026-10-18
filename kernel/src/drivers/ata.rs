//! QEMU PIIX3 IDE primary master, LBA28, single-sector polling PIO.
//! No DMA, IRQs, ATAPI or arbitrary disks. Only the exact disposable serial
//! and volume geometry are accepted. Boot disk remains on q35 AHCI.
use crate::arch::x86_64::{
    inb, outb,
    storage_io::{inl, inw, outl, outw},
};
use storage::{BlockDevice, DISK_SECTORS, Error, Result, SECTOR};

#[derive(Debug)]
pub struct Ata {
    sectors: u64,
}

fn read(port: u16) -> u8 {
    // SAFETY: storage initialization/lock is the sole user of this primary
    // IDE channel. IRQs are disabled at the device; no handler accesses it.
    unsafe { inb(port) }
}
fn write(port: u16, value: u8) {
    // SAFETY: same exclusive primary-channel ownership as read; call sites
    // supply ATA register values for the bounded PIO protocol.
    unsafe { outb(port, value) }
}
fn config(address: u32) -> u32 {
    // SAFETY: only this boot-time routine uses PCI configuration ports;
    // the valid mechanism-1 address is formed from bus0 device/function.
    unsafe { outl(0xcf8, address) };
    // SAFETY: reads the configuration dword selected immediately above.
    unsafe { inl(0xcfc) }
}
fn wait(drq: bool) -> Result<()> {
    let deadline = crate::time::uptime_ns().saturating_add(1_000_000_000);
    for _ in 0..2_000_000 {
        let s = read(0x1f7);
        if s == 0 || s == 0xff {
            return Err(Error::Io);
        }
        if s & 0x80 == 0 {
            if s & 0x21 != 0 {
                return Err(Error::Io);
            }
            if (s & 8 != 0) == drq {
                return Ok(());
            }
        }
        if crate::time::uptime_ns() >= deadline {
            break;
        }
        core::hint::spin_loop();
    }
    Err(Error::Io)
}
fn delay() {
    for _ in 0..4 {
        let _ = read(0x3f6);
    }
}

impl Ata {
    /// Called exactly once at boot before user processes. This driver is
    /// subsequently moved into the single storage lock, never cloned.
    pub fn discover() -> Result<Self> {
        let mut found = None;
        for dev in 0..32u32 {
            for fun in 0..8u32 {
                let address = 0x8000_0000 | (dev << 11) | (fun << 8);
                if config(address) == 0x7010_8086 {
                    if found.is_some() {
                        return Err(Error::Corrupt);
                    }
                    found = Some(address);
                }
            }
        }
        let address = found.ok_or(Error::NotFound)?;
        let class = config(address | 8);
        if class >> 16 != 0x0101 || class & 0x500 != 0 {
            return Err(Error::Io);
        }
        let command = (config(address | 4) as u16 | 1) & !4;
        // SAFETY: boot-time sole PCI owner; selects the discovered PIIX3
        // command register and enables only I/O decoding (no bus mastering).
        unsafe { outl(0xcf8, address | 4) };
        // SAFETY: 16-bit write avoids clearing PCI status W1C bits.
        unsafe { outw(0xcfc, command) };
        let timing = config(address | 0x40) | 0x8000;
        // SAFETY: selects PIIX IDE timing register of the discovered device.
        unsafe { outl(0xcf8, address | 0x40) };
        // SAFETY: preserves timings and enables primary compatibility port.
        unsafe { outl(0xcfc, timing) };
        write(0x3f6, 2); // nIEN: polling only
        write(0x1f6, 0xa0);
        delay();
        for p in 0x1f2..=0x1f5 {
            write(p, 0);
        }
        write(0x1f7, 0xec);
        delay();
        wait(true)?;
        let mut words = [0u16; 256];
        for w in &mut words {
            // SAFETY: IDENTIFY asserted DRQ for exactly 256 words, and
            // this routine exclusively owns the primary data register.
            *w = unsafe { inw(0x1f0) };
        }
        wait(false)?;
        let mut serial = [0; 20];
        for i in 0..10 {
            serial[i * 2..i * 2 + 2].copy_from_slice(&words[10 + i].to_be_bytes());
        }
        let sectors = u64::from(words[60]) | (u64::from(words[61]) << 16);
        if &serial != b"RUSTOS-DISPOSABLE   "
            || sectors != DISK_SECTORS
            || words[49] & (1 << 9) == 0
            || words[83] & (1 << 12) == 0
        {
            return Err(Error::Corrupt);
        }
        Ok(Self { sectors })
    }
    fn select(&self, lba: u64, command: u8) -> Result<()> {
        if lba >= self.sectors || lba >= (1 << 28) {
            return Err(Error::Io);
        }
        wait(false)?;
        write(0x1f6, 0xe0 | ((lba >> 24) as u8 & 15));
        delay();
        write(0x1f2, 1);
        write(0x1f3, lba as u8);
        write(0x1f4, (lba >> 8) as u8);
        write(0x1f5, (lba >> 16) as u8);
        write(0x1f7, command);
        delay();
        wait(true)
    }
}
impl BlockDevice for Ata {
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn read(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> Result<()> {
        self.select(lba, 0x20)?;
        for b in out.as_chunks_mut::<2>().0 {
            // SAFETY: selected READ SECTORS has DRQ for one 512-byte block;
            // exclusive driver ownership; exactly 256 data-port reads.
            b.copy_from_slice(&unsafe { inw(0x1f0) }.to_le_bytes());
        }
        delay();
        wait(false)
    }
    fn write(&mut self, lba: u64, data: &[u8; SECTOR]) -> Result<()> {
        self.select(lba, 0x30)?;
        for b in data.as_chunks::<2>().0 {
            // SAFETY: selected WRITE SECTORS has DRQ for one 512-byte block;
            // exclusive driver ownership; exactly 256 data-port writes.
            unsafe { outw(0x1f0, u16::from_le_bytes([b[0], b[1]])) };
        }
        delay();
        wait(false)
    }
    fn flush(&mut self) -> Result<()> {
        wait(false)?;
        write(0x1f7, 0xe7);
        delay();
        wait(false)
    }
}
