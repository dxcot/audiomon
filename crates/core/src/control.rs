//! Control-channel handshake (TCP). The host connects to the phone and sends [`Hello`]; the
//! phone answers with [`Accept`]. The connection then stays open: when it closes, streaming stops.
//! While streaming, the phone sends [`Ping`] and the host answers [`Pong`] to sync clocks.

use std::io::{self, Read, Write};

use crate::PROTO_VERSION;

const HELLO_MAGIC: &[u8; 4] = b"ARST";
const ACCEPT_MAGIC: &[u8; 4] = b"ARSA";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub sample_rate: u32,
    pub channels: u16,
    pub packet_frames: u16,
}

impl Hello {
    pub const LEN: usize = 14;

    pub fn new(sample_rate: u32, channels: u16, packet_frames: u16) -> Self {
        Self { version: PROTO_VERSION, sample_rate, channels, packet_frames }
    }

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let mut b = [0u8; Self::LEN];
        b[0..4].copy_from_slice(HELLO_MAGIC);
        b[4..6].copy_from_slice(&self.version.to_le_bytes());
        b[6..10].copy_from_slice(&self.sample_rate.to_le_bytes());
        b[10..12].copy_from_slice(&self.channels.to_le_bytes());
        b[12..14].copy_from_slice(&self.packet_frames.to_le_bytes());
        w.write_all(&b)
    }

    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut b = [0u8; Self::LEN];
        r.read_exact(&mut b)?;
        if &b[0..4] != HELLO_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not an audiorast host"));
        }
        Ok(Self {
            version: u16::from_le_bytes([b[4], b[5]]),
            sample_rate: u32::from_le_bytes([b[6], b[7], b[8], b[9]]),
            channels: u16::from_le_bytes([b[10], b[11]]),
            packet_frames: u16::from_le_bytes([b[12], b[13]]),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Status {
    Ok = 0,
    UnsupportedVersion = 1,
    UnsupportedFormat = 2,
    Busy = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accept {
    pub version: u16,
    pub status: Status,
}

impl Accept {
    pub const LEN: usize = 8;

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let mut b = [0u8; Self::LEN];
        b[0..4].copy_from_slice(ACCEPT_MAGIC);
        b[4..6].copy_from_slice(&self.version.to_le_bytes());
        b[6..8].copy_from_slice(&(self.status as u16).to_le_bytes());
        w.write_all(&b)
    }

    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut b = [0u8; Self::LEN];
        r.read_exact(&mut b)?;
        if &b[0..4] != ACCEPT_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not an audiorast receiver"));
        }
        let status = match u16::from_le_bytes([b[6], b[7]]) {
            0 => Status::Ok,
            1 => Status::UnsupportedVersion,
            2 => Status::UnsupportedFormat,
            3 => Status::Busy,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown status")),
        };
        Ok(Self { version: u16::from_le_bytes([b[4], b[5]]), status })
    }
}

/// Phone → host clock probe, carrying the phone's send time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub phone_us: u64,
}

impl Ping {
    pub const LEN: usize = 9;
    const TAG: u8 = b'P';

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0] = Self::TAG;
        b[1..9].copy_from_slice(&self.phone_us.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; Self::LEN]) -> io::Result<Self> {
        if b[0] != Self::TAG {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected ping"));
        }
        Ok(Self { phone_us: u64::from_le_bytes(b[1..9].try_into().unwrap()) })
    }
}

/// Host → phone answer: the ping's phone time echoed, plus the host time when answering.
/// The host clock is the one used for packet timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub phone_us: u64,
    pub host_us: u64,
}

impl Pong {
    pub const LEN: usize = 17;
    const TAG: u8 = b'Q';

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0] = Self::TAG;
        b[1..9].copy_from_slice(&self.phone_us.to_le_bytes());
        b[9..17].copy_from_slice(&self.host_us.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; Self::LEN]) -> io::Result<Self> {
        if b[0] != Self::TAG {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected pong"));
        }
        Ok(Self {
            phone_us: u64::from_le_bytes(b[1..9].try_into().unwrap()),
            host_us: u64::from_le_bytes(b[9..17].try_into().unwrap()),
        })
    }
}

/// Estimates `host_clock - phone_clock` from ping/pong round trips. Keeps the estimate from the
/// fastest recent round trip, where the one-way delays are smallest and most symmetric.
#[derive(Debug, Default)]
pub struct ClockSync {
    /// (round trip µs, offset µs) of recent samples.
    samples: [(i64, i64); 8],
    len: usize,
    next: usize,
}

impl ClockSync {
    /// `sent`/`received` are phone times of the ping and pong, `host` the host time in the pong.
    pub fn add(&mut self, sent: u64, host: u64, received: u64) {
        let rtt = received as i64 - sent as i64;
        let offset = host as i64 - (sent as i64 + rtt / 2);
        self.samples[self.next] = (rtt, offset);
        self.next = (self.next + 1) % self.samples.len();
        self.len = (self.len + 1).min(self.samples.len());
    }

    fn best(&self) -> Option<(i64, i64)> {
        self.samples[..self.len].iter().copied().min_by_key(|&(rtt, _)| rtt)
    }

    /// Host time minus phone time, in µs.
    pub fn offset_us(&self) -> Option<i64> {
        self.best().map(|(_, o)| o)
    }

    pub fn rtt_us(&self) -> Option<i64> {
        self.best().map(|(r, _)| r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong_roundtrip() {
        let p = Ping { phone_us: 123 };
        assert_eq!(Ping::decode(&p.encode()).unwrap(), p);
        let q = Pong { phone_us: 123, host_us: 456 };
        assert_eq!(Pong::decode(&q.encode()).unwrap(), q);
        assert!(Pong::decode(&[0u8; Pong::LEN]).is_err());
    }

    #[test]
    fn clock_sync_prefers_fastest_round_trip() {
        let mut s = ClockSync::default();
        // Host clock is 1_000_000 µs ahead. Slow, asymmetric sample first, then a fast one.
        s.add(100, 1_000_100 + 900, 1_100);
        s.add(2_000, 1_002_000 + 50, 2_100);
        assert_eq!(s.rtt_us(), Some(100));
        assert_eq!(s.offset_us(), Some(1_000_000));
    }

    #[test]
    fn hello_roundtrip() {
        let h = Hello::new(48_000, 2, 480);
        let mut buf = Vec::new();
        h.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), Hello::LEN);
        assert_eq!(Hello::read_from(&mut buf.as_slice()).unwrap(), h);
    }

    #[test]
    fn accept_roundtrip() {
        let a = Accept { version: PROTO_VERSION, status: Status::Busy };
        let mut buf = Vec::new();
        a.write_to(&mut buf).unwrap();
        assert_eq!(Accept::read_from(&mut buf.as_slice()).unwrap(), a);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Hello::read_from(&mut &b"GET / HTTP/1."[..]).is_err());
    }
}
