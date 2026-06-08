//! # Host ↔ in-VM control protocol (RFC-0004 §4, F2b)
//!
//! A tiny framed request/response spoken over vsock between the host and the
//! guest [agent-runner](../../guest-runner): one request per connection,
//! `[op: u8][len: u64 LE][payload]`. Shared verbatim by both ends so the wire
//! format cannot drift.

use std::io::{self, Read, Write};

/// Operation / status tags.
pub mod op {
    /// Request: deploy a `Package` (payload = package bytes).
    pub const DEPLOY: u8 = 1;
    /// Request: report agent health (payload empty).
    pub const HEALTH: u8 = 2;
    /// Request: mint and return a fresh `Package` (payload empty).
    pub const CHECKPOINT: u8 = 3;
    /// Request: reset/stop the VM (payload empty).
    pub const SHUTDOWN: u8 = 4;
    /// Response status: success.
    pub const OK: u8 = 0;
    /// Response status: error (payload = message).
    pub const ERR: u8 = 1;
}

/// Write one `[tag][len][payload]` frame.
pub fn write_frame<W: Write>(w: &mut W, tag: u8, payload: &[u8]) -> io::Result<()> {
    w.write_all(&[tag])?;
    w.write_all(&(payload.len() as u64).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Read one `[tag][len][payload]` frame.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag)?;
    let mut len = [0u8; 8];
    r.read_exact(&mut len)?;
    let mut buf = vec![0u8; u64::from_le_bytes(len) as usize];
    r.read_exact(&mut buf)?;
    Ok((tag[0], buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips() {
        let mut buf = Vec::new();
        write_frame(&mut buf, op::DEPLOY, b"payload").unwrap();
        let (tag, payload) = read_frame(&mut &buf[..]).unwrap();
        assert_eq!(tag, op::DEPLOY);
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn empty_payload_round_trips() {
        let mut buf = Vec::new();
        write_frame(&mut buf, op::HEALTH, &[]).unwrap();
        let (tag, payload) = read_frame(&mut &buf[..]).unwrap();
        assert_eq!(tag, op::HEALTH);
        assert!(payload.is_empty());
    }
}
