//! Single-producer, single-consumer byte ring for interrupt handlers.
//!
//! The producer is one interrupt handler (or a group of handlers that cannot
//! interrupt each other); the consumer is the kernel's main loop. Neither side
//! ever waits or takes a lock, so the main loop being interrupted halfway
//! through [`ByteRing::pop`] cannot deadlock the handler.
//!
//! Overflow policy: when the ring is full, [`ByteRing::push`] drops the *new*
//! byte and increments [`ByteRing::dropped`]. Older bytes are never
//! overwritten, because the producer must not touch slots the consumer may be
//! reading. Consumers that decode multi-byte sequences watch `dropped` and
//! resynchronize when it changes.
//!
//! Memory safety does not depend on the single-producer/single-consumer
//! contract (there is no `unsafe` code); breaking it can only lose or repeat
//! bytes.

use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

/// A ring of `N` bytes. `N` must be a power of two (checked at compile time
/// when [`ByteRing::new`] is evaluated).
#[derive(Debug)]
pub struct ByteRing<const N: usize> {
    slots: [AtomicU8; N],
    /// Total bytes ever consumed (wrapping). Written only by the consumer.
    head: AtomicUsize,
    /// Total bytes ever produced (wrapping). Written only by the producer.
    tail: AtomicUsize,
    dropped: AtomicU32,
}

impl<const N: usize> ByteRing<N> {
    const CAPACITY_OK: () = assert!(N.is_power_of_two() && N >= 2);

    pub const fn new() -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            slots: [const { AtomicU8::new(0) }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicU32::new(0),
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    /// Producer side: append `byte`. Returns `false` (and counts a drop) if
    /// the ring is full.
    pub fn push(&self, byte: u8) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= N {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.slots[tail % N].store(byte, Ordering::Relaxed);
        // Release: the slot write above is visible before the new tail.
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side: take the oldest byte.
    pub fn pop(&self) -> Option<u8> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let byte = self.slots[head % N].load(Ordering::Relaxed);
        // Release: the slot read above happens before the producer may reuse it.
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(byte)
    }

    /// Bytes waiting (a snapshot; may change immediately).
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head).min(N)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes dropped because the ring was full, since creation (wrapping).
    pub fn dropped(&self) -> u32 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl<const N: usize> Default for ByteRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::vec::Vec;

    #[test]
    fn fifo_order_and_empty() {
        let ring = ByteRing::<4>::new();
        assert!(ring.is_empty());
        assert_eq!(ring.pop(), None);
        for b in 1..=3 {
            assert!(ring.push(b));
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.pop(), Some(1));
        assert!(ring.push(4));
        assert!(ring.push(5));
        assert_eq!(
            [ring.pop(), ring.pop(), ring.pop(), ring.pop(), ring.pop()],
            [Some(2), Some(3), Some(4), Some(5), None]
        );
    }

    #[test]
    fn full_ring_drops_new_bytes_and_keeps_old_ones() {
        let ring = ByteRing::<8>::new();
        for b in 0..20u8 {
            ring.push(b);
        }
        assert_eq!(ring.len(), 8);
        assert_eq!(ring.dropped(), 12);
        let got: Vec<u8> = core::iter::from_fn(|| ring.pop()).collect();
        assert_eq!(got, (0..8).collect::<Vec<_>>());
        // Usable again after draining.
        assert!(ring.push(99));
        assert_eq!(ring.pop(), Some(99));
    }

    #[test]
    fn indices_wrap_without_losing_bytes() {
        let ring = ByteRing::<2>::new();
        ring.head.store(usize::MAX - 1, Ordering::Relaxed);
        ring.tail.store(usize::MAX - 1, Ordering::Relaxed);
        for b in 0..10u8 {
            assert!(ring.push(b));
            assert_eq!(ring.pop(), Some(b));
        }
        assert_eq!(ring.dropped(), 0);
    }

    #[test]
    fn concurrent_producer_and_consumer_account_for_every_byte() {
        const TOTAL: u32 = 200_000;
        let ring = Arc::new(ByteRing::<64>::new());
        let producer = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let mut accepted = Vec::new();
                for i in 0..TOTAL {
                    let byte = (i % 251) as u8;
                    if ring.push(byte) {
                        accepted.push(byte);
                    }
                }
                accepted
            })
        };
        let mut received = Vec::new();
        loop {
            match ring.pop() {
                Some(b) => received.push(b),
                None if producer.is_finished() => {
                    // Drain anything published just before the producer ended.
                    received.extend(core::iter::from_fn(|| ring.pop()));
                    break;
                }
                None => std::hint::spin_loop(),
            }
        }
        let accepted = producer.join().unwrap();
        assert_eq!(received, accepted, "bytes arrive in order, none duplicated");
        assert_eq!(accepted.len() as u32 + ring.dropped(), TOTAL);
    }
}
