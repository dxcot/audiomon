//! Turns received datagrams into a continuous sample stream: orders by sequence number,
//! drops late packets, and conceals lost ones so the playout timeline stays intact.

use crate::SampleSink;
use crate::packet::Header;

/// Gaps larger than this many packets are treated as a stream restart, not loss.
const MAX_CONCEAL_PACKETS: u32 = 10;
/// Fade-in length after a concealed gap, in frames (~1.3 ms at 48 kHz).
const FADE_IN_FRAMES: usize = 64;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DepacketizerStats {
    pub received: u64,
    /// Packets that never arrived and were concealed.
    pub lost: u64,
    /// Packets that arrived after a newer one (dropped).
    pub late: u64,
    /// Packets dropped because the ring buffer was full.
    pub overflows: u64,
    /// Datagrams with an inconsistent length (dropped).
    pub malformed: u64,
    /// Sequence jumps too large to conceal.
    pub resyncs: u64,
}

pub struct Depacketizer {
    channels: usize,
    expected: Option<u32>,
    /// Last good packet, used to conceal losses.
    prev: Vec<f32>,
    cur: Vec<f32>,
    stats: DepacketizerStats,
}

impl Depacketizer {
    pub fn new(channels: usize) -> Self {
        Self { channels, expected: None, prev: Vec::new(), cur: Vec::new(), stats: Default::default() }
    }

    pub fn stats(&self) -> DepacketizerStats {
        self.stats
    }

    /// Forgets the sequence state, e.g. after a reconnect.
    pub fn reset(&mut self) {
        self.expected = None;
        self.prev.clear();
    }

    pub fn push(&mut self, datagram: &[u8], sink: &mut impl SampleSink) {
        let Some((header, payload)) = Header::read(datagram) else {
            self.stats.malformed += 1;
            return;
        };
        let samples = header.frames as usize * self.channels;
        if !payload.is_empty() && payload.len() != samples * 4 {
            self.stats.malformed += 1;
            return;
        }

        let mut fade_in = false;
        if let Some(expected) = self.expected {
            let gap = header.seq.wrapping_sub(expected) as i32;
            if gap < 0 {
                self.stats.late += 1;
                return;
            }
            if gap > 0 {
                fade_in = true;
                if gap as u32 <= MAX_CONCEAL_PACKETS {
                    self.stats.lost += gap as u64;
                    self.conceal(gap as usize, sink);
                } else {
                    self.stats.resyncs += 1;
                }
            }
        }
        self.stats.received += 1;
        self.expected = Some(header.seq.wrapping_add(1));

        self.cur.clear();
        if payload.is_empty() {
            self.cur.resize(samples, 0.0);
        } else {
            self.cur.extend(payload.as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)));
        }
        if fade_in {
            let frames = FADE_IN_FRAMES.min(header.frames as usize);
            for (i, frame) in self.cur.chunks_exact_mut(self.channels).take(frames).enumerate() {
                let g = i as f32 / frames as f32;
                frame.iter_mut().for_each(|s| *s *= g);
            }
        }
        self.emit(sink);
        std::mem::swap(&mut self.prev, &mut self.cur);
    }

    /// Fills `packets` missing packets by repeating the previous one while fading it out.
    fn conceal(&mut self, packets: usize, sink: &mut impl SampleSink) {
        let frames = self.prev.len() / self.channels;
        if frames == 0 {
            return;
        }
        let total = frames * packets;
        self.cur.clear();
        for i in 0..total {
            let g = 1.0 - i as f32 / frames.min(total) as f32;
            let src = &self.prev[(i % frames) * self.channels..][..self.channels];
            self.cur.extend(src.iter().map(|s| s * g.max(0.0)));
        }
        self.emit(sink);
    }

    fn emit(&mut self, sink: &mut impl SampleSink) {
        if sink.free() < self.cur.len() {
            self.stats.overflows += 1;
            return;
        }
        sink.write(&self.cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::encode;
    use std::collections::VecDeque;

    fn datagram(seq: u32, samples: &[f32]) -> Vec<u8> {
        let mut out = Vec::new();
        encode(&Header { seq, frames: (samples.len() / 2) as u32, host_ts_us: 0 }, samples, &mut out);
        out
    }

    #[test]
    fn in_order() {
        let mut d = Depacketizer::new(2);
        let mut ring = VecDeque::new();
        d.push(&datagram(5, &[1.0, 2.0]), &mut ring);
        d.push(&datagram(6, &[3.0, 4.0]), &mut ring);
        assert_eq!(ring, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(d.stats().received, 2);
    }

    #[test]
    fn late_packet_dropped() {
        let mut d = Depacketizer::new(2);
        let mut ring = VecDeque::new();
        d.push(&datagram(10, &[1.0, 1.0]), &mut ring);
        d.push(&datagram(9, &[2.0, 2.0]), &mut ring);
        assert_eq!(ring.len(), 2);
        assert_eq!(d.stats().late, 1);
    }

    #[test]
    fn loss_is_concealed_with_same_length() {
        let mut d = Depacketizer::new(2);
        let mut ring = VecDeque::new();
        let pkt = vec![0.5f32; 2 * 100];
        d.push(&datagram(0, &pkt), &mut ring);
        d.push(&datagram(3, &pkt), &mut ring); // 1 and 2 lost
        assert_eq!(ring.len(), 4 * 200);
        assert_eq!(d.stats().lost, 2);
        // Concealment fades out, and the packet after the gap fades in from zero.
        assert_eq!(ring[200], 0.5);
        assert_eq!(ring[200 + 2 * 99], 0.5 * (1.0 - 99.0 / 100.0));
        assert_eq!(ring[400], 0.0);
        assert_eq!(ring[600], 0.0);
    }

    #[test]
    fn wraparound() {
        let mut d = Depacketizer::new(2);
        let mut ring = VecDeque::new();
        d.push(&datagram(u32::MAX, &[1.0, 1.0]), &mut ring);
        d.push(&datagram(0, &[1.0, 1.0]), &mut ring);
        assert_eq!(d.stats(), DepacketizerStats { received: 2, ..Default::default() });
    }

    #[test]
    fn silence_packet() {
        let mut d = Depacketizer::new(2);
        let mut ring = VecDeque::new();
        let mut out = Vec::new();
        encode(&Header { seq: 0, frames: 3, host_ts_us: 0 }, &[], &mut out);
        d.push(&out, &mut ring);
        assert_eq!(ring, [0.0; 6]);
    }

    #[test]
    fn malformed() {
        let mut d = Depacketizer::new(2);
        let mut ring = VecDeque::new();
        let mut out = datagram(0, &[1.0, 1.0]);
        out.pop();
        d.push(&out, &mut ring);
        d.push(&[0u8; 3], &mut ring);
        assert_eq!(d.stats().malformed, 2);
        assert!(ring.is_empty());
    }
}
