//! Shared, OS-independent logic for audiomon.
//!
//! Receiver pipeline:
//! `UDP datagram → Depacketizer → SampleSink (ring buffer) → SampleSource → Playout → audio device`.
//! The depacketizer runs on the network thread, the playout in the real-time audio callback.

pub mod control;
pub mod depacketizer;
pub mod jitter;
pub mod packet;
pub mod playout;
pub mod stream;

use std::collections::VecDeque;

/// Default control (TCP) port.
pub const CONTROL_PORT: u16 = 47800;
/// Default audio (UDP) port.
pub const AUDIO_PORT: u16 = 47801;
/// UDP port where the phone answers discovery broadcasts from PCs on the same network.
pub const DISCOVERY_PORT: u16 = 47802;
/// Discovery request (PC → broadcast) and answer (phone → PC).
pub const DISCOVERY_REQUEST: &[u8] = b"AMON?1";
pub const DISCOVERY_REPLY: &[u8] = b"AMON!1";
/// Protocol version sent in HELLO.
pub const PROTO_VERSION: u16 = 1;

/// Read side of the sample ring between network and audio threads. Units are interleaved samples.
pub trait SampleSource {
    fn available(&self) -> usize;
    /// Reads up to `out.len()` samples, returns how many were read.
    fn read(&mut self, out: &mut [f32]) -> usize;
    /// Discards up to `n` samples.
    fn skip(&mut self, n: usize);
}

/// Write side of the sample ring. Units are interleaved samples.
pub trait SampleSink {
    fn free(&self) -> usize;
    /// Writes as many samples as fit, returns how many were written.
    fn write(&mut self, data: &[f32]) -> usize;
}

impl SampleSource for VecDeque<f32> {
    fn available(&self) -> usize {
        self.len()
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.len());
        for (o, s) in out.iter_mut().zip(self.drain(..n)) {
            *o = s;
        }
        n
    }

    fn skip(&mut self, n: usize) {
        self.drain(..n.min(self.len()));
    }
}

impl SampleSink for VecDeque<f32> {
    fn free(&self) -> usize {
        usize::MAX
    }

    fn write(&mut self, data: &[f32]) -> usize {
        self.extend(data);
        data.len()
    }
}
