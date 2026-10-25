//! Endpoint bytes → the one format RAOP carries: 44100 Hz, 16-bit, stereo, LE.
//!
//! Platform-neutral on purpose. This is the only code that can corrupt audio
//! without failing: reading a float32 endpoint as int16 is what produced loud
//! static, and no amount of protocol correctness rescues a wrong conversion. It
//! is therefore kept free of Windows types and covered by tests.

/// 16-bit stereo, the format RAOP carries.
pub const BYTES_PER_FRAME: usize = 4;
/// Rate RAOP carries.
pub const SAMPLE_RATE: u32 = 44_100;
/// Bytes of converted audio per second.
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * BYTES_PER_FRAME;

/// Converts whatever a capture endpoint produces into 44.1k/16-bit/stereo LE.
///
/// Stateful: the resampling phase and the previous frame survive between calls,
/// so feeding the same audio in different chunk sizes produces the same output.
pub struct Converter {
    rate: u32,
    channels: u16,
    bits: u16,
    is_float: bool,
    phase: f64,
    prev: (f32, f32),
}

impl Converter {
    pub fn new(rate: u32, channels: u16, bits: u16, is_float: bool) -> Self {
        Self {
            rate: if rate == 0 { SAMPLE_RATE } else { rate },
            channels: channels.max(1),
            bits,
            is_float,
            phase: 0.0,
            prev: (0.0, 0.0),
        }
    }

    /// Bytes per interleaved frame as the endpoint produces them.
    fn frame_width(&self) -> usize {
        (self.bits / 8) as usize * self.channels as usize
    }

    /// Decode one interleaved frame to `(left, right)` in -1.0 .. 1.0.
    ///
    /// Only the first two channels are used. A mono endpoint is duplicated, and a
    /// frame too short to hold a second channel repeats the first rather than
    /// dropping to silence.
    fn decode_frame(&self, frame: &[u8]) -> (f32, f32) {
        let width = (self.bits / 8) as usize;
        if width == 0 {
            return (0.0, 0.0);
        }

        let read = |channel: usize| -> Option<f32> {
            let start = channel * width;
            let s = frame.get(start..start + width)?;
            Some(match (self.is_float, width) {
                (true, 4) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                (true, 8) => {
                    f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
                }
                (false, 2) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32_768.0,
                // 24-bit is stored in three bytes; shift into a 32-bit word so the
                // sign extends, then scale by 2^23.
                (false, 3) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
                (false, 4) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                // An unknown width would otherwise be silently loud: treat it as
                // silence and let the caller notice an empty stream.
                _ => return Some(0.0),
            })
        };

        let left = read(0).unwrap_or(0.0);
        let right = if self.channels >= 2 {
            read(1).unwrap_or(left)
        } else {
            left
        };
        (left, right)
    }

    /// Convert a block of endpoint bytes, appending 44.1k/16/stereo LE to `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let width = self.frame_width();
        if bytes.is_empty() || width == 0 {
            return;
        }

        // A partial trailing frame is carried to the next call by the caller
        // reading whole frames from the endpoint; ignoring it here keeps the
        // channel alignment intact.
        let mut frames: Vec<(f32, f32)> = Vec::with_capacity(bytes.len() / width);
        for frame in bytes.chunks_exact(width) {
            frames.push(self.decode_frame(frame));
        }
        if frames.is_empty() {
            return;
        }

        let step = self.rate as f64 / SAMPLE_RATE as f64;
        let mut pos = self.phase;
        while pos < frames.len() as f64 {
            let idx = pos.floor() as usize;
            let frac = pos - idx as f64;
            let a = if idx == 0 { self.prev } else { frames[idx - 1] };
            let b = frames[idx.min(frames.len() - 1)];
            let l = a.0 as f64 + (b.0 as f64 - a.0 as f64) * frac;
            let r = a.1 as f64 + (b.1 as f64 - a.1 as f64) * frac;
            out.extend_from_slice(&((l.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
            out.extend_from_slice(&((r.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
            pos += step;
        }

        self.phase = pos - frames.len() as f64;
        self.prev = frames[frames.len() - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two properties of the converter shape every expectation below:
    //   * the first output frame primes the interpolator and is silence;
    //   * the last input frame is retained, because the next call needs it as the
    //     left neighbour of its first frame.
    // So N input frames produce N output frames: one silence, then frames 0..N-2.

    fn i16_le(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn as_i16(bytes: &[u8]) -> Vec<i16> {
        bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    fn f32_le(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn run(mut conv: Converter, bytes: &[u8]) -> Vec<i16> {
        let mut out = Vec::new();
        conv.push(bytes, &mut out);
        as_i16(&out)
    }

    #[test]
    fn int16_stereo_at_44100_is_passed_through() {
        let input = [1000i16, -1000, 2000, -2000, 3000, -3000, 4000, -4000];
        let samples = run(Converter::new(44_100, 2, 16, false), &i16_le(&input));

        assert_eq!(samples.len(), 8, "one output frame per input frame");
        assert_eq!(
            &samples[..2],
            &[0, 0],
            "first frame primes the interpolator"
        );

        // Decoding divides by 32768 and encoding multiplies by 32767, so an exact
        // round trip is off by at most one LSB. That asymmetry is deliberate: it is
        // what keeps a full-scale negative sample inside the range.
        for (got, want) in samples[2..].iter().zip(input[..6].iter()) {
            assert!(
                (i32::from(*got) - i32::from(*want)).abs() <= 1,
                "got {got}, want about {want}"
            );
        }
    }

    #[test]
    fn float32_is_not_read_as_int16() {
        // The bug that produced loud static: float32 bits taken as int16.
        let bytes = f32_le(&[0.5, -0.5, 0.25, -0.25]);
        let samples = run(Converter::new(44_100, 2, 32, true), &bytes);
        assert_eq!(&samples[2..], &[16383, -16383]);
    }

    #[test]
    fn float64_is_accepted() {
        let mut bytes = Vec::new();
        for v in [0.5f64, 0.5, 0.0, 0.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let samples = run(Converter::new(44_100, 2, 64, true), &bytes);
        assert_eq!(&samples[2..], &[16383, 16383]);
    }

    #[test]
    fn int24_is_sign_extended() {
        // Full-scale positive, then full-scale negative, then a zero frame.
        let bytes = [0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0, 0, 0, 0, 0, 0];
        let samples = run(Converter::new(44_100, 2, 24, false), &bytes);
        assert_eq!(samples[2], 32766, "positive full scale, truncated");
        assert_eq!(samples[3], -32767, "negative full scale");
    }

    #[test]
    fn int32_is_scaled_by_full_range() {
        let mut bytes = Vec::new();
        for v in [0x4000_0000i32, -0x4000_0000, 0, 0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let samples = run(Converter::new(44_100, 2, 32, false), &bytes);
        assert_eq!(&samples[2..], &[16383, -16383]);
    }

    #[test]
    fn mono_is_duplicated_to_both_channels() {
        let samples = run(
            Converter::new(44_100, 1, 16, false),
            &i16_le(&[1234, -1234, 0]),
        );
        assert_eq!(&samples[2..], &[1233, 1233, -1233, -1233]);
    }

    #[test]
    fn extra_channels_are_ignored() {
        let bytes = f32_le(&[0.5, -0.5, 9.0, 9.0, 0.0, 0.0, 0.0, 0.0]);
        let samples = run(Converter::new(44_100, 4, 32, true), &bytes);
        assert_eq!(&samples[2..], &[16383, -16383]);
    }

    #[test]
    fn clipping_is_bounded() {
        let bytes = f32_le(&[2.0, -2.0, 0.0, 0.0]);
        let samples = run(Converter::new(44_100, 2, 32, true), &bytes);
        assert_eq!(&samples[2..], &[32767, -32767]);
    }

    #[test]
    fn resampling_48k_produces_the_right_number_of_frames() {
        // 480 frames at 48k is 10 ms, which must become 441 frames at 44.1k. The
        // boundary lands exactly on 480.0 in real arithmetic, so floating point may
        // include one extra frame; the rate is verified over a longer run below.
        let bytes = vec![0u8; 480 * 8];
        let samples = run(Converter::new(48_000, 2, 32, true), &bytes);
        let frames = samples.len() / 2;
        assert!(
            frames.abs_diff(441) <= 1,
            "expected ~441 frames, got {frames}"
        );
    }

    #[test]
    fn resampling_preserves_duration_across_many_chunks() {
        // One second of 48k audio fed as 100 chunks of 10 ms must still be one
        // second of output. This is the drift test: a per-chunk reset loses frames.
        let mut conv = Converter::new(48_000, 2, 32, true);
        let mut out = Vec::new();
        let chunk = vec![0u8; 48_000 / 100 * 8];
        for _ in 0..100 {
            conv.push(&chunk, &mut out);
        }
        let frames = out.len() / BYTES_PER_FRAME;
        assert!(
            frames.abs_diff(SAMPLE_RATE as usize) <= 1,
            "expected ~44100 frames, got {frames}"
        );
    }

    #[test]
    fn output_does_not_depend_on_chunk_boundaries() {
        // A resampler that resets per chunk drifts and clicks. This fails if the
        // phase or the previous frame is dropped between calls.
        let source: Vec<u8> = (0..400u32)
            .flat_map(|i| {
                let v = (i as f32 * 0.01).sin() * 0.8;
                ((v * 32767.0) as i16).to_le_bytes()
            })
            .collect();

        let mut whole = Converter::new(44_100, 1, 16, false);
        let mut whole_out = Vec::new();
        whole.push(&source, &mut whole_out);

        let mut split = Converter::new(44_100, 1, 16, false);
        let mut split_out = Vec::new();
        for chunk in source.chunks(7 * 2) {
            split.push(chunk, &mut split_out);
        }

        assert_eq!(whole_out, split_out);
    }

    #[test]
    fn empty_input_produces_nothing() {
        let mut conv = Converter::new(44_100, 2, 16, false);
        let mut out = Vec::new();
        conv.push(&[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn nonsense_geometry_does_not_panic() {
        let mut conv = Converter::new(0, 0, 16, false);
        let mut out = Vec::new();
        conv.push(&[0u8; 8], &mut out);
        assert_eq!(out.len() % BYTES_PER_FRAME, 0);
    }
}
