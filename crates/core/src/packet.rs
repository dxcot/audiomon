//! Audio datagram framing.
//!
//! Layout (little-endian): `[u32 seq][u32 frames][u64 host_ts_us][payload]`.
//! The payload is interleaved f32 samples (`frames * channels * 4` bytes), or empty to mean
//! `frames` frames of silence.

pub const HEADER_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Monotonic packet counter; wraps around.
    pub seq: u32,
    /// Number of audio frames (samples per channel) in the payload.
    pub frames: u32,
    /// Host clock time (µs) at which the first frame of this packet was produced.
    pub host_ts_us: u64,
}

impl Header {
    pub fn write(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.seq.to_le_bytes());
        buf[4..8].copy_from_slice(&self.frames.to_le_bytes());
        buf[8..16].copy_from_slice(&self.host_ts_us.to_le_bytes());
    }

    /// Splits a datagram into its header and payload. Returns `None` if it is too short.
    pub fn read(buf: &[u8]) -> Option<(Header, &[u8])> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let header = Header {
            seq: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            frames: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            host_ts_us: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        };
        Some((header, &buf[HEADER_LEN..]))
    }
}

/// Builds a datagram into `out` (cleared first). Pass empty `samples` for a silence packet.
pub fn encode(header: &Header, samples: &[f32], out: &mut Vec<u8>) {
    out.clear();
    out.resize(HEADER_LEN, 0);
    header.write(out);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let h = Header { seq: 0xDEAD_BEEF, frames: 480, host_ts_us: 1_234_567_890 };
        let mut buf = [0u8; HEADER_LEN + 4];
        h.write(&mut buf);
        buf[HEADER_LEN..].copy_from_slice(&[1, 2, 3, 4]);
        let (parsed, payload) = Header::read(&buf).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn too_short() {
        assert!(Header::read(&[0u8; HEADER_LEN - 1]).is_none());
    }

    #[test]
    fn encode_samples() {
        let h = Header { seq: 7, frames: 1, host_ts_us: 0 };
        let mut out = Vec::new();
        encode(&h, &[0.5, -0.25], &mut out);
        assert_eq!(out.len(), HEADER_LEN + 8);
        let (parsed, payload) = Header::read(&out).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(f32::from_le_bytes(payload[4..8].try_into().unwrap()), -0.25);
    }
}
