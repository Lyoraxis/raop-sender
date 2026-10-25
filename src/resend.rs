//! Receiver-requested retransmission of RTP packets.
//!
//! A RAOP receiver reports a lost packet on the control port and expects the
//! sender to resend it, from the same RTP socket, with the original sequence
//! number and timestamp. Ignoring those requests is legal but turns a lossy link
//! into audible dropouts; serving them costs one ring buffer.
//!
//! Platform-neutral and tested: the packet layout and the ring's behaviour under
//! sequence wraparound are both easy to get subtly wrong and impossible to see by
//! ear.

use std::collections::VecDeque;
use std::sync::Arc;

/// First two bytes of a resend request.
const REQUEST: [u8; 2] = [0x80, 0xD5];
/// Bytes three and four are always 0x00 0x07 on this protocol.
const REQUEST_LENGTH: [u8; 2] = [0x00, 0x07];
/// A resend request is exactly this many bytes.
pub const REQUEST_BYTES: usize = 8;

/// Parse a receiver's resend request into `(first_sequence, count)`.
///
/// Returns `None` for anything that is not a well-formed request, including the
/// timing packets that arrive on the same socket — this must never treat a timing
/// probe as a demand to resend.
pub fn parse_request(buf: &[u8]) -> Option<(u16, u16)> {
    if buf.len() < REQUEST_BYTES || buf[0..2] != REQUEST || buf[2..4] != REQUEST_LENGTH {
        return None;
    }
    let first = u16::from_be_bytes([buf[4], buf[5]]);
    let count = u16::from_be_bytes([buf[6], buf[7]]);
    if count == 0 {
        return None;
    }
    Some((first, count))
}

/// Build a resend request. Only used by the test harness, which needs to speak
/// the receiver's side of the exchange.
pub fn build_request(first: u16, count: u16) -> [u8; REQUEST_BYTES] {
    let mut out = [0u8; REQUEST_BYTES];
    out[0..2].copy_from_slice(&REQUEST);
    out[2..4].copy_from_slice(&REQUEST_LENGTH);
    out[4..6].copy_from_slice(&first.to_be_bytes());
    out[6..8].copy_from_slice(&count.to_be_bytes());
    out
}

/// The last few seconds of sent packets, keyed by sequence number.
///
/// Bounded on purpose: a receiver that asks for something older than the ring
/// held simply does not get it, which is the same outcome as not implementing this
/// at all, but without unbounded memory growth.
pub struct ResendRing {
    capacity: usize,
    entries: VecDeque<(u16, Arc<Vec<u8>>)>,
}

impl ResendRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    /// Remember a packet. `seq` must be the packet's own sequence number.
    pub fn push(&mut self, seq: u16, packet: Arc<Vec<u8>>) {
        if self.capacity == 1 {
            self.entries.clear();
        } else if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((seq, packet));
    }

    pub fn get(&self, seq: u16) -> Option<Arc<Vec<u8>>> {
        self.entries
            .iter()
            .rev()
            .find(|(s, _)| *s == seq)
            .map(|(_, p)| p.clone())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Newest sequence number held, for diagnostics.
    pub fn newest(&self) -> Option<u16> {
        self.entries.back().map(|(s, _)| *s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_request_is_parsed() {
        let request = build_request(1234, 3);
        assert_eq!(parse_request(&request), Some((1234, 3)));
    }

    #[test]
    fn sequence_numbers_are_big_endian() {
        // 0x0102 must not read as 0x0201; the wire is network order.
        let mut request = build_request(0x0102, 0x0304);
        assert_eq!(request[4], 0x01);
        assert_eq!(request[5], 0x02);
        assert_eq!(parse_request(&request), Some((0x0102, 0x0304)));
        request[4] = 0xFF;
        assert_eq!(parse_request(&request), Some((0xFF02, 0x0304)));
    }

    #[test]
    fn a_timing_packet_is_not_a_resend_request() {
        // Timing requests share the control socket; treating one as a resend
        // request would have the sender blast stale audio at the receiver.
        let timing = [0x80u8, 0xD2, 0x00, 0x07, 0, 0, 0, 0];
        assert_eq!(parse_request(&timing), None);
    }

    #[test]
    fn short_or_foreign_packets_are_ignored() {
        assert_eq!(parse_request(&[]), None);
        assert_eq!(parse_request(&[0x80, 0xD5, 0x00, 0x07]), None);
        assert_eq!(parse_request(&[0x00; 8]), None);
        let mut wrong_length = build_request(1, 1);
        wrong_length[3] = 0x08;
        assert_eq!(parse_request(&wrong_length), None);
    }

    #[test]
    fn a_zero_count_request_is_ignored() {
        assert_eq!(parse_request(&build_request(10, 0)), None);
    }

    #[test]
    fn the_ring_returns_what_it_stored() {
        let mut ring = ResendRing::new(4);
        for seq in 0..4u16 {
            ring.push(seq, Arc::new(vec![seq as u8]));
        }
        assert_eq!(ring.get(0).map(|p| p[0]), Some(0));
        assert_eq!(ring.get(3).map(|p| p[0]), Some(3));
        assert!(ring.get(9).is_none());
    }

    #[test]
    fn the_ring_evicts_oldest_first_and_keeps_the_newest() {
        let mut ring = ResendRing::new(3);
        for seq in 0..5u16 {
            ring.push(seq, Arc::new(vec![seq as u8]));
        }
        assert_eq!(ring.len(), 3);
        assert!(ring.get(0).is_none(), "oldest must be gone");
        assert!(ring.get(1).is_none());
        assert_eq!(ring.get(2).map(|p| p[0]), Some(2));
        assert_eq!(ring.newest(), Some(4));
    }

    #[test]
    fn sequence_wraparound_is_handled() {
        // 65534, 65535, 0, 1 — a receiver can ask for any of them.
        let mut ring = ResendRing::new(8);
        for seq in [65534u16, 65535, 0, 1] {
            ring.push(seq, Arc::new(seq.to_le_bytes().to_vec()));
        }
        for seq in [65534u16, 65535, 0, 1] {
            let packet = ring.get(seq).expect("wrapped sequence must be found");
            assert_eq!(packet.as_slice(), &seq.to_le_bytes());
        }
        assert_eq!(ring.newest(), Some(1));
    }

    #[test]
    fn a_capacity_of_one_still_behaves() {
        let mut ring = ResendRing::new(1);
        ring.push(7, Arc::new(vec![7]));
        ring.push(8, Arc::new(vec![8]));
        assert_eq!(ring.len(), 1);
        assert!(ring.get(7).is_none());
        assert!(ring.get(8).is_some());
    }
}
