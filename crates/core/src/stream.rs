//! Framing for byte-stream transports (USB accessory bulk pipe), where datagram boundaries
//! are not preserved: each message is `[u32 len LE][kind u8][payload]`, `len` = 1 + payload.

/// Audio datagram (same bytes as a UDP packet, see [`crate::packet`]).
pub const KIND_AUDIO: u8 = b'A';
/// Clock-sync ping/pong (see [`crate::control::Ping`] / [`crate::control::Pong`]).
pub const KIND_PING: u8 = b'P';
pub const KIND_PONG: u8 = b'Q';
/// Phone → PC human-readable status line (UTF-8).
pub const KIND_STATS: u8 = b'S';
/// PC → phone at the start of every stream ([`crate::control::Hello`] bytes): the phone resets
/// its session state, since a byte stream has no connection setup of its own.
pub const KIND_HELLO: u8 = b'H';

/// Largest accepted message; anything bigger means the stream is out of sync.
const MAX_MESSAGE: usize = 64 * 1024;

/// Appends one framed message to `out`.
pub fn encode(kind: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
}

/// Reassembles framed messages from arbitrary chunks of the stream.
#[derive(Default)]
pub struct Deframer {
    buf: Vec<u8>,
    start: usize,
}

impl Deframer {
    /// Feeds a chunk read from the stream.
    pub fn push(&mut self, chunk: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        self.buf.extend_from_slice(chunk);
    }

    /// Next complete message as `(kind, payload)`. `Err` means the stream is corrupt.
    pub fn next_message(&mut self) -> Result<Option<(u8, &[u8])>, &'static str> {
        let pending = &self.buf[self.start..];
        if pending.len() < 4 {
            self.compact();
            return Ok(None);
        }
        let len = u32::from_le_bytes(pending[..4].try_into().unwrap()) as usize;
        if len == 0 || len > MAX_MESSAGE {
            return Err("bad frame length");
        }
        if pending.len() < 4 + len {
            self.compact();
            return Ok(None);
        }
        let at = self.start + 4;
        self.start = at + len;
        Ok(Some((self.buf[at], &self.buf[at + 1..at + len])))
    }

    /// Drops consumed bytes so the buffer doesn't grow without bound.
    fn compact(&mut self) {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_arbitrary_chunking() {
        let mut stream = Vec::new();
        encode(KIND_AUDIO, &[1, 2, 3], &mut stream);
        encode(KIND_PONG, &[9; 17], &mut stream);
        encode(KIND_AUDIO, &[], &mut stream);

        for chunk_size in 1..stream.len() {
            let mut d = Deframer::default();
            let mut got = Vec::new();
            for chunk in stream.chunks(chunk_size) {
                d.push(chunk);
                while let Some((kind, payload)) = d.next_message().unwrap() {
                    got.push((kind, payload.to_vec()));
                }
            }
            assert_eq!(
                got,
                vec![(KIND_AUDIO, vec![1, 2, 3]), (KIND_PONG, vec![9; 17]), (KIND_AUDIO, vec![])],
                "chunk size {chunk_size}"
            );
        }
    }

    #[test]
    fn rejects_garbage_length() {
        let mut d = Deframer::default();
        d.push(&[0xFF, 0xFF, 0xFF, 0x7F, 0]);
        assert!(d.next_message().is_err());
    }
}
