//! Arrival-delay statistics for sizing the playout buffer (in the spirit of WebRTC's NetEQ).
//!
//! Each packet's lateness is measured relative to the earliest arrival seen recently. A
//! histogram of that lateness forgets old packets exponentially, and the buffer margin is a high
//! quantile of it: enough to absorb all but the rarest spikes, without sizing for the worst one.

const BUCKET_MS: f64 = 0.25;
const BUCKETS: usize = 160; // 40 ms; later arrivals land in the last bucket.
/// Sliding minimum = min over `MIN_BLOCKS` blocks of `MIN_BLOCK_PACKETS` packets each.
const MIN_BLOCKS: usize = 5;
const MIN_BLOCK_PACKETS: u32 = 200;

#[derive(Debug, Clone)]
pub struct DelayHistogram {
    /// Probability mass per lateness bucket; sums to ~1 once warmed up.
    mass: Vec<f64>,
    /// Weight kept by old packets on each new one (1 - 1/N, N ≈ packets remembered).
    forget: f64,
    block_min: [f64; MIN_BLOCKS],
    block: usize,
    in_block: u32,
    count: u64,
    next_seq: Option<u32>,
}

impl DelayHistogram {
    /// `memory_packets`: roughly how many recent packets shape the estimate.
    pub fn new(memory_packets: u32) -> Self {
        Self {
            mass: vec![0.0; BUCKETS],
            forget: 1.0 - 1.0 / memory_packets.max(1) as f64,
            block_min: [f64::INFINITY; MIN_BLOCKS],
            block: 0,
            in_block: 0,
            count: 0,
            next_seq: None,
        }
    }

    /// Adds a received packet, accounting for lost ones before it: a lost packet's audio is
    /// concealed only when this packet arrives, so for buffering purposes it arrived now,
    /// `k` packet lengths late.
    pub fn add_packet(&mut self, seq: u32, lag_ms: f64, packet_ms: f64) {
        if let Some(expected) = self.next_seq {
            let gap = seq.wrapping_sub(expected) as i32;
            if gap < 0 {
                return; // late or duplicate: dropped by the depacketizer anyway
            }
            // Larger gaps are stream restarts, not loss (see the depacketizer).
            if gap <= 10 {
                for k in 1..=gap {
                    self.add(lag_ms + k as f64 * packet_ms);
                }
            }
        }
        self.next_seq = Some(seq.wrapping_add(1));
        self.add(lag_ms);
    }

    /// Adds one packet's arrival lag (arrival time minus send time, any constant offset), in ms.
    pub fn add(&mut self, lag_ms: f64) {
        if self.in_block == MIN_BLOCK_PACKETS {
            self.block = (self.block + 1) % MIN_BLOCKS;
            self.block_min[self.block] = f64::INFINITY;
            self.in_block = 0;
        }
        self.in_block += 1;
        self.block_min[self.block] = self.block_min[self.block].min(lag_ms);
        let base = self.block_min.iter().copied().fold(f64::INFINITY, f64::min);

        let late = (lag_ms - base).max(0.0);
        let bucket = ((late / BUCKET_MS) as usize).min(BUCKETS - 1);
        self.mass.iter_mut().for_each(|m| *m *= self.forget);
        self.mass[bucket] += 1.0 - self.forget;
        self.count += 1;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Lateness (ms) that a fraction `q` of recent packets did not exceed.
    pub fn quantile(&self, q: f64) -> f64 {
        let total: f64 = self.mass.iter().sum();
        if total <= 0.0 {
            return 0.0;
        }
        let mut acc = 0.0;
        for (i, m) in self.mass.iter().enumerate() {
            acc += m;
            if acc >= q * total {
                return (i + 1) as f64 * BUCKET_MS;
            }
        }
        BUCKETS as f64 * BUCKET_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_arrivals_need_no_margin() {
        let mut h = DelayHistogram::new(1000);
        for _ in 0..2000 {
            h.add(12.0);
        }
        assert_eq!(h.quantile(0.995), BUCKET_MS);
    }

    #[test]
    fn quantile_ignores_rare_spikes() {
        let mut h = DelayHistogram::new(2000);
        for i in 0..4000 {
            // 0.2% of packets 8 ms late, 3% 2 ms late, the rest on time.
            let lag = if i % 500 == 0 { 8.0 } else if i % 33 == 0 { 2.0 } else { 0.0 };
            h.add(5.0 + lag);
        }
        let q = h.quantile(0.995);
        assert!((2.0..=2.5).contains(&q), "q99.5 = {q}");
        assert!(h.quantile(0.9995) >= 8.0);
    }

    #[test]
    fn baseline_shift_is_forgotten() {
        let mut h = DelayHistogram::new(500);
        for _ in 0..1000 {
            h.add(5.0);
        }
        // Clock offset re-sync shifts every lag by +3 ms: within the sliding-min memory
        // packets look late, after it the new baseline is learned.
        for _ in 0..(MIN_BLOCKS as u32 * MIN_BLOCK_PACKETS + 3000) {
            h.add(8.0);
        }
        assert!(h.quantile(0.995) <= 0.5);
    }
}
