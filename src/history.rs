//! Fixed-capacity ring buffers for sample history.
//!
//! Sized at compile time and allocated once at startup, so sampling never
//! touches the allocator and the working set never grows over uptime.

/// The chart displays 240 readings (~2 hours at the 30 s default). Keeping
/// older, undisplayed samples only wastes memory: 480 bytes per sensor suffice.
pub const CAPACITY: usize = 240;

/// Sentinel for "no reading at this slot" (sensor absent or read failed).
pub const NO_VALUE: i16 = i16::MIN;

/// Temperatures are stored as tenths of a degree Celsius in an i16: enough
/// range for -3276.7..3276.7 C, rounded to the nearest tenth. The timeline
/// uses the same bounded storage with u64 millisecond timestamps.
#[derive(Clone)]
pub struct History<T = i16> {
    buf: Box<[T; CAPACITY]>,
    /// Index the next sample will be written to.
    head: usize,
    /// Number of valid samples, saturating at CAPACITY.
    len: usize,
}

impl<T: Copy + Default> History<T> {
    pub fn new() -> Self {
        Self {
            buf: Box::new([T::default(); CAPACITY]),
            head: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, value: T) {
        self.buf[self.head] = value;
        self.head = (self.head + 1) % CAPACITY;
        if self.len < CAPACITY {
            self.len += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Oldest-to-newest iterator over the last `n` samples.
    pub fn iter_recent(&self, n: usize) -> impl DoubleEndedIterator<Item = T> + '_ {
        let n = n.min(self.len);
        let start = (self.head + CAPACITY - n) % CAPACITY;
        (0..n).map(move |k| self.buf[(start + k) % CAPACITY])
    }
}

impl History<u64> {
    pub fn time_bounds(&self, n: usize) -> Option<(u64, u64)> {
        let mut times = self.iter_recent(n);
        let first = times.next()?;
        Some((first, times.next_back().unwrap_or(first)))
    }
}

impl History<i16> {
    /// Most recent sample, if any and if it is a real reading.
    #[allow(dead_code)]
    pub fn last(&self) -> Option<i16> {
        if self.len == 0 {
            return None;
        }
        let i = (self.head + CAPACITY - 1) % CAPACITY;
        let v = self.buf[i];
        (v != NO_VALUE).then_some(v)
    }

    /// (min, max) over the last `n` real samples.
    pub fn range_recent(&self, n: usize) -> Option<(i16, i16)> {
        let mut lo = i16::MAX;
        let mut hi = i16::MIN;
        let mut any = false;
        for v in self.iter_recent(n) {
            if v == NO_VALUE {
                continue;
            }
            any = true;
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
        any.then_some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_newest_samples_in_order_after_wrap() {
        let mut h = History::<u64>::new();
        for i in 0..CAPACITY as u64 + 7 {
            h.push(i);
        }
        assert_eq!(h.len(), CAPACITY);
        assert_eq!(h.time_bounds(CAPACITY), Some((7, CAPACITY as u64 + 6)));
        assert_eq!(
            h.iter_recent(3).collect::<Vec<_>>(),
            vec![
                CAPACITY as u64 + 4,
                CAPACITY as u64 + 5,
                CAPACITY as u64 + 6
            ]
        );
    }

    #[test]
    fn time_bounds_do_not_invent_an_interval_before_first_sample() {
        let mut h = History::<u64>::new();
        assert_eq!(h.time_bounds(240), None);
        h.push(12_000);
        assert_eq!(h.time_bounds(240), Some((12_000, 12_000)));
        h.push(42_750);
        h.push(43_800);
        assert_eq!(h.time_bounds(2), Some((42_750, 43_800)));
    }

    #[test]
    fn temperature_range_ignores_failed_reads() {
        let mut h = History::new();
        h.push(NO_VALUE);
        assert_eq!(h.range_recent(10), None);
        h.push(-4);
        h.push(658);
        h.push(NO_VALUE);
        assert_eq!(h.range_recent(10), Some((-4, 658)));
        assert_eq!(h.last(), None);
    }
}
