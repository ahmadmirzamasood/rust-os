//! Deterministic host mutation testing, not coverage-guided fuzzing.
use storage::{BlockDevice, DiskFs, Kind, SECTOR, Vfs};

#[derive(Clone, Debug)]
struct Disk(Vec<[u8; SECTOR]>);
impl BlockDevice for Disk {
    fn sectors(&self) -> u64 {
        self.0.len() as u64
    }
    fn read(&mut self, lba: u64, out: &mut [u8; SECTOR]) -> storage::Result<()> {
        *out = *self.0.get(lba as usize).ok_or(storage::Error::Io)?;
        Ok(())
    }
    fn write(&mut self, lba: u64, bytes: &[u8; SECTOR]) -> storage::Result<()> {
        *self.0.get_mut(lba as usize).ok_or(storage::Error::Io)? = *bytes;
        Ok(())
    }
    fn flush(&mut self) -> storage::Result<()> {
        Ok(())
    }
}

fn random(seed: &mut u64) -> usize {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed as usize
}

fn mutations(seed: &mut u64, bytes: &mut [u8]) {
    for _ in 0..1 + random(seed) % 8 {
        let at = random(seed) % bytes.len();
        bytes[at] ^= (random(seed) as u8) | 1;
    }
}

pub fn run() -> Result<(), String> {
    let mut seed = 0x5255_5354_4f53_u64;
    let limits = exec::elf::Limits {
        start: os_abi::layout::IMAGE_START,
        end: os_abi::layout::IMAGE_END,
        max_bytes: os_abi::layout::IMAGE_MAX_BYTES,
    };
    // Valid seeds reach deeper checks than random magic bytes.
    let mut elf = vec![0; 128];
    elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    elf[16..18].copy_from_slice(&2u16.to_le_bytes());
    elf[18..20].copy_from_slice(&62u16.to_le_bytes());
    elf[20..24].copy_from_slice(&1u32.to_le_bytes());
    elf[24..32].copy_from_slice(&limits.start.to_le_bytes());
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());
    elf[64..68].copy_from_slice(&1u32.to_le_bytes());
    elf[68..72].copy_from_slice(&5u32.to_le_bytes());
    elf[80..88].copy_from_slice(&limits.start.to_le_bytes());
    elf[96..104].copy_from_slice(&128u64.to_le_bytes());
    elf[104..112].copy_from_slice(&4096u64.to_le_bytes());
    assert!(exec::elf::parse(&elf, &limits).is_ok());
    let bundle = exec::bundle::build(&[("seed", &elf)]).map_err(|e| format!("{e:?}"))?;
    let mut packet = vec![0; 17];
    packet[0] = 1;
    packet[1] = 1;
    packet[8..12].copy_from_slice(&32u32.to_le_bytes());
    packet[12..16].copy_from_slice(&32u32.to_le_bytes());
    packet[16] = b'A';
    assert!(desktop::display::decode(&packet).is_ok());
    for case in 0..100_000 {
        for (kind, original) in [&elf, &bundle, &packet].into_iter().enumerate() {
            let mut bytes = original.clone();
            mutations(&mut seed, &mut bytes);
            if case % 3 == 0 {
                bytes.truncate(random(&mut seed) % (bytes.len() + 1));
            }
            match kind {
                0 => {
                    if let Ok(image) = exec::elf::parse(&bytes, &limits) {
                        assert!(image.mapped_bytes() <= limits.max_bytes);
                        for s in image.segments() {
                            assert!(!(s.writable && s.executable));
                            assert!(s.file_offset + s.file_size <= bytes.len() as u64);
                        }
                    }
                }
                1 => {
                    if let Ok(b) = exec::bundle::Bundle::parse(&bytes) {
                        assert_eq!(b.iter().count(), b.len());
                    }
                }
                _ => {
                    let _ = desktop::display::decode(&bytes);
                }
            }
        }
    }
    let mut disk = Disk(vec![[0; SECTOR]; storage::DISK_SECTORS as usize]);
    storage::format(&mut disk).map_err(|e| format!("{e:?}"))?;
    let mut fs = DiskFs::mount(disk).map_err(|e| format!("{e:?}"))?;
    for cycle in 0..10_000u64 {
        let id = fs
            .create("/stress", Kind::File)
            .map_err(|e| format!("{e:?}"))?;
        let data = cycle.to_le_bytes();
        assert_eq!(fs.write(id, 0, &data), Ok(data.len()));
        fs.rename("/stress", "/renamed")
            .map_err(|e| format!("{e:?}"))?;
        fs.sync().map_err(|e| format!("{e:?}"))?;
        fs = DiskFs::mount(fs.into_device()).map_err(|e| format!("{e:?}"))?;
        let mut got = [0; 8];
        let reopened = fs.lookup("/renamed").map_err(|e| format!("{e:?}"))?.0;
        assert_eq!(fs.read(reopened, 0, &mut got), Ok(8));
        assert_eq!(got, data);
        fs.remove("/renamed").map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            fs.read(reopened, 0, &mut got),
            Err(storage::Error::NotFound)
        );
    }
    fs.create("/seed", Kind::Directory)
        .map_err(|e| format!("{e:?}"))?;
    let seed_file = fs
        .create("/seed/file", Kind::File)
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        fs.write(seed_file, 0, &[0x5a; storage::FILE_MAX]),
        Ok(storage::FILE_MAX)
    );
    fs.sync().map_err(|e| format!("{e:?}"))?;
    let baseline = fs.into_device();
    let control = DiskFs::mount(baseline.clone()).map_err(|e| format!("seed mount: {e:?}"))?;
    let mut seed_bytes = [0; storage::FILE_MAX];
    let control_id = control
        .lookup("/seed/file")
        .map_err(|e| format!("seed lookup: {e:?}"))?
        .0;
    assert_eq!(
        control.read(control_id, 0, &mut seed_bytes),
        Ok(storage::FILE_MAX)
    );
    assert_eq!(seed_bytes, [0x5a; storage::FILE_MAX]);
    for case in 0..20_000 {
        let mut disk = baseline.clone();
        let sector = match case % 3 {
            0 => 0,
            1 => 1 + random(&mut seed) % 2,
            _ => 1 + storage::NODES + 8 + random(&mut seed) % 8,
        };
        mutations(&mut seed, &mut disk.0[sector]);
        // Half the metadata mutations repair checksums to reach structural
        // validation. Data mutations exercise file checksum rejection.
        if case % 2 == 0 && sector <= storage::NODES {
            let hash = disk.0[sector][..508].iter().fold(2166136261u32, |h, b| {
                (h ^ u32::from(*b)).wrapping_mul(16777619)
            });
            disk.0[sector][508..].copy_from_slice(&hash.to_le_bytes());
        }
        let _ = DiskFs::mount(disk);
    }
    eprintln!(
        "robustness PASS: seed 0x525553544f53; 300000 ELF/bundle/display mutations, 20000 RSFS mutations, 10000 sync/remount/rename/remove cycles"
    );
    Ok(())
}
