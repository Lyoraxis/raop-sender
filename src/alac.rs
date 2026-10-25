//! ALAC encoding for RAOP.
//!
//! The receiver decodes payload type 96 as AppleLossless, so PCM must be
//! encoded before it is packetised. 352 frames per packet, 16-bit stereo.

use alac_encoder::{AlacEncoder, FormatDescription};

pub const FRAMES_PER_PACKET: u32 = 352;
pub const SAMPLE_RATE: f64 = 44_100.0;

pub struct Alac {
    encoder: AlacEncoder,
    input: FormatDescription,
    scratch: Vec<u8>,
}

impl Default for Alac {
    fn default() -> Self {
        Self::new()
    }
}

impl Alac {
    pub fn new() -> Self {
        let output = FormatDescription::alac(SAMPLE_RATE, FRAMES_PER_PACKET, 2);
        let encoder = AlacEncoder::new(&output);
        let scratch = vec![0u8; output.max_packet_size()];
        Self {
            encoder,
            input: FormatDescription::pcm::<i16>(SAMPLE_RATE, 2),
            scratch,
        }
    }

    /// Encode interleaved little-endian i16 stereo into one ALAC frame.
    pub fn encode(&mut self, pcm_le: &[u8]) -> &[u8] {
        let n = self.encoder.encode(&self.input, pcm_le, &mut self.scratch);
        &self.scratch[..n]
    }
}

/// The `a=fmtp` parameter string: the ALAC magic cookie, space separated.
pub const FMTP: &str = "352 0 16 40 10 14 2 255 0 0 44100";

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let v = ((i as f32 * 0.05).sin() * 8000.0) as i16;
            out.extend_from_slice(&v.to_le_bytes()); // left
            out.extend_from_slice(&v.to_le_bytes()); // right
        }
        out
    }

    #[test]
    fn encodes_a_full_packet() {
        let mut enc = Alac::new();
        let pcm = sine(FRAMES_PER_PACKET as usize);
        let frame = enc.encode(&pcm);
        assert!(!frame.is_empty(), "encoder produced nothing");
        assert!(
            frame.len() < pcm.len(),
            "ALAC frame ({} B) should be smaller than the raw PCM ({} B)",
            frame.len(),
            pcm.len()
        );
    }

    #[test]
    fn encoding_is_deterministic() {
        // The encoder is stateful across packets (adaptive prediction), so the
        // same input encoded twice in sequence may differ. Two fresh encoders
        // must agree, which is what makes output reproducible.
        let pcm = sine(FRAMES_PER_PACKET as usize);
        let a = Alac::new().encode(&pcm).to_vec();
        let b = Alac::new().encode(&pcm).to_vec();
        assert_eq!(a, b, "fresh encoders must agree on the same input");
    }

    #[test]
    fn sequential_packets_are_all_frames() {
        let mut enc = Alac::new();
        for i in 0..8 {
            let frame = enc.encode(&sine(FRAMES_PER_PACKET as usize));
            assert!(!frame.is_empty(), "packet {i} encoded to nothing");
            assert!(frame.len() < FRAMES_PER_PACKET as usize * 4);
        }
    }
}
