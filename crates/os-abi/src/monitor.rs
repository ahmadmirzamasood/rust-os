//! Read-only system snapshot v1. Explicit little-endian wire format; no pointers.
pub const MAX_PROCESSES: usize = 16;
pub const SIZE: usize = 32 + MAX_PROCESSES * 48;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// 1 = alive, 2 = exited but still referenced.
    pub state: u8,
    pub name_len: u8,
    pub handles: u16,
    pub cpu_ns: u64,
    pub name: [u8; 32],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub count: usize,
    pub uptime_ns: u64,
    pub free_bytes: u64,
    pub threads: u32,
    pub processes: [Process; MAX_PROCESSES],
}

impl Snapshot {
    pub fn encode(&self, out: &mut [u8; SIZE]) {
        out.fill(0);
        out[..4].copy_from_slice(&1u32.to_le_bytes());
        let count = self.count.min(MAX_PROCESSES);
        out[4..8].copy_from_slice(&(count as u32).to_le_bytes());
        out[8..16].copy_from_slice(&self.uptime_ns.to_le_bytes());
        out[16..24].copy_from_slice(&self.free_bytes.to_le_bytes());
        out[24..28].copy_from_slice(&self.threads.to_le_bytes());
        for (p, row) in self.processes[..count]
            .iter()
            .zip(out[32..].as_chunks_mut::<48>().0.iter_mut())
        {
            row[..4].copy_from_slice(&p.pid.to_le_bytes());
            row[4] = p.state;
            row[5] = p.name_len;
            row[6..8].copy_from_slice(&p.handles.to_le_bytes());
            row[8..16].copy_from_slice(&p.cpu_ns.to_le_bytes());
            row[16..48].copy_from_slice(&p.name);
        }
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SIZE || bytes[..4] != 1u32.to_le_bytes() || bytes[28..32] != [0; 4] {
            return None;
        }
        let count = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        if count > MAX_PROCESSES || bytes[32 + count * 48..].iter().any(|b| *b != 0) {
            return None;
        }
        let mut out = Self {
            count,
            uptime_ns: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            free_bytes: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
            threads: u32::from_le_bytes(bytes[24..28].try_into().ok()?),
            ..Self::default()
        };
        for (p, row) in out.processes[..count]
            .iter_mut()
            .zip(bytes[32..].as_chunks::<48>().0.iter())
        {
            let len = row[5] as usize;
            if !(1..=32).contains(&len)
                || !(1..=2).contains(&row[4])
                || row[16..16 + len].iter().any(|b| !(32..127).contains(b))
                || row[16 + len..48].iter().any(|b| *b != 0)
            {
                return None;
            }
            *p = Process {
                pid: u32::from_le_bytes(row[..4].try_into().ok()?),
                state: row[4],
                name_len: row[5],
                handles: u16::from_le_bytes(row[6..8].try_into().ok()?),
                cpu_ns: u64::from_le_bytes(row[8..16].try_into().ok()?),
                name: row[16..48].try_into().ok()?,
            };
            if p.pid == 0 {
                return None;
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_round_trip_and_malformed_records() {
        let mut s = Snapshot {
            count: 1,
            free_bytes: 8192,
            threads: 3,
            ..Snapshot::default()
        };
        s.processes[0] = Process {
            pid: 1,
            state: 1,
            name_len: 5,
            cpu_ns: 9000,
            ..Process::default()
        };
        s.processes[0].name[..5].copy_from_slice(b"shell");
        let mut bytes = [0; SIZE];
        s.encode(&mut bytes);
        assert_eq!(Snapshot::decode(&bytes), Some(s));
        for (at, bad) in [
            (0, 2),
            (4, 17),
            (28, 1),
            (36, 3),
            (37, 33),
            (53, 1),
            (SIZE - 1, 1),
        ] {
            let mut broken = bytes;
            broken[at] = bad;
            assert_eq!(Snapshot::decode(&broken), None, "byte {at}");
        }
        assert_eq!(Snapshot::decode(&bytes[..SIZE - 1]), None);
    }
}
