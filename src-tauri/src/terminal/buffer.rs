use std::collections::VecDeque;

/// Byte ring buffer with a monotonically increasing sequence number.
///
/// `written` counts every byte ever appended; a consumer that has seen up to
/// `seq` can ask for the delta. If the consumer is older than what the ring
/// still retains, a full replay is returned (consumer should reset its view).
pub struct Backlog {
    buf: VecDeque<u8>,
    cap: usize,
    written: u64,
}

impl Backlog {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.min(64 * 1024)),
            cap,
            written: 0,
        }
    }

    /// Appends bytes and returns the new `written` watermark.
    pub fn append(&mut self, bytes: &[u8]) -> u64 {
        if self.cap == 0 {
            self.written += bytes.len() as u64;
            return self.written;
        }
        for &b in bytes {
            self.buf.push_back(b);
        }
        while self.buf.len() > self.cap {
            self.buf.pop_front();
        }
        self.written += bytes.len() as u64;
        self.written
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    /// Returns `(reset_hint, bytes)` — bytes produced after `seq`.
    /// `reset_hint` is true when the requested range partially fell out of the
    /// ring, i.e. the returned bytes start mid-stream and the consumer should
    /// clear its terminal before writing them.
    pub fn replay_after(&self, seq: u64) -> (bool, Vec<u8>) {
        if seq >= self.written {
            return (false, Vec::new());
        }
        let retained = self.buf.len() as u64;
        let missing = self.written - seq;
        if missing > retained {
            (true, self.buf.iter().copied().collect())
        } else {
            let skip = (retained - missing) as usize;
            (false, self.buf.iter().skip(skip).copied().collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_delta_and_full() {
        let mut b = Backlog::new(10);
        b.append(b"0123456789");
        assert_eq!(b.replay_after(0), (false, b"0123456789".to_vec()));
        assert_eq!(b.replay_after(4), (false, b"456789".to_vec()));
        assert_eq!(b.replay_after(10), (false, Vec::<u8>::new()));

        // Ring wraps: keep last 10 of 20 written.
        b.append(b"ABCDEFGHIJ");
        assert_eq!(b.written(), 20);
        let (reset, data) = b.replay_after(0);
        assert!(reset, "caller older than ring must get reset hint");
        assert_eq!(data, b"ABCDEFGHIJ".to_vec());
        let (reset, data) = b.replay_after(15);
        assert!(!reset);
        assert_eq!(data, b"FGHIJ".to_vec());
    }

    #[test]
    fn zero_cap_still_counts() {
        let mut b = Backlog::new(0);
        b.append(b"abc");
        assert_eq!(b.written(), 3);
        assert_eq!(b.replay_after(0), (true, Vec::<u8>::new()));
    }
}
