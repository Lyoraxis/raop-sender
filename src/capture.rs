//! WASAPI capture of the virtual cable, converted to the fixed format RAOP needs.
//!
//! We no longer assume anything about the endpoint: the mix format is read,
//! logged, and converted here (sample type, bit depth, channel count and sample
//! rate). Sending raw endpoint bytes as if they were 44.1k/16/stereo is what
//! produced loud static.

#![cfg(windows)]

use crate::session::Log;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 16-bit stereo, the format RAOP carries.
pub const BYTES_PER_FRAME: usize = 4;
pub const SAMPLE_RATE: u32 = 44_100;
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * BYTES_PER_FRAME;

pub fn start(
    log: Log,
    alive: Arc<AtomicBool>,
    queue: Arc<Mutex<VecDeque<u8>>>,
    device_filter: Option<String>,
) -> Result<(), String> {
    std::thread::spawn(move || {
        let logger = log.clone();
        if let Err(e) = run(log, alive, queue, device_filter) {
            logger(format!("capture stopped: {e}"));
        }
    });
    Ok(())
}

/// Pick the capture device whose friendly name contains `filter`, falling back
/// to a loopback of the default render endpoint when no filter is given.
fn pick_device(
    enumerator: &wasapi::DeviceEnumerator,
    filter: &Option<String>,
) -> Result<wasapi::Device, String> {
    use wasapi::Direction;

    match filter {
        None => enumerator
            .get_default_device(&Direction::Render)
            .map_err(|e| format!("default render device: {e}")),
        Some(needle) => {
            let lower = needle.to_lowercase();
            let collection = enumerator
                .get_device_collection(&Direction::Capture)
                .map_err(|e| format!("capture devices: {e}"))?;
            let count = collection
                .get_nbr_devices()
                .map_err(|e| format!("capture device count: {e}"))?;

            let mut seen: Vec<String> = Vec::new();
            for idx in 0..count {
                match collection.get_device_at_index(idx) {
                    Ok(device) => {
                        let name = device.get_friendlyname().unwrap_or_default();
                        if name.to_lowercase().contains(&lower) {
                            return Ok(device);
                        }
                        seen.push(name);
                    }
                    Err(_) => break,
                }
            }
            Err(format!(
                "no capture device matching {needle:?}; saw: {}",
                seen.join(", ")
            ))
        }
    }
}

/// Converts whatever the endpoint produces into 44100 Hz / 16-bit / stereo LE.
struct Converter {
    rate: u32,
    channels: u16,
    bits: u16,
    is_float: bool,
    phase: f64,
    prev: (f32, f32),
}

impl Converter {
    fn new(rate: u32, channels: u16, bits: u16, is_float: bool) -> Self {
        Self {
            rate,
            channels,
            bits,
            is_float,
            phase: 0.0,
            prev: (0.0, 0.0),
        }
    }

    /// Decode one interleaved frame to (left, right) in -1.0 .. 1.0.
    fn decode_frame(&self, frame: &[u8]) -> (f32, f32) {
        let width = (self.bits / 8) as usize;
        if width == 0 {
            return (0.0, 0.0);
        }
        let mut ch: Vec<f32> = Vec::with_capacity(self.channels as usize);
        for c in 0..self.channels as usize {
            let start = c * width;
            if start + width > frame.len() {
                break;
            }
            let s = &frame[start..start + width];
            let v = if self.is_float && width == 4 {
                f32::from_le_bytes([s[0], s[1], s[2], s[3]])
            } else if self.is_float && width == 8 {
                f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
            } else {
                match width {
                    2 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
                    3 => {
                        let raw = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
                        raw as f32 / 8_388_608.0
                    }
                    4 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                    _ => 0.0,
                }
            };
            ch.push(v);
        }
        match ch.len() {
            0 => (0.0, 0.0),
            1 => (ch[0], ch[0]),
            _ => (ch[0], ch[1]),
        }
    }

    /// Convert a block of endpoint bytes, appending 44.1k/16/stereo LE to `out`.
    fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let width = (self.bits / 8) as usize * self.channels as usize;
        if width == 0 {
            return;
        }

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

fn run(
    log: Log,
    alive: Arc<AtomicBool>,
    queue: Arc<Mutex<VecDeque<u8>>>,
    device_filter: Option<String>,
) -> Result<(), String> {
    use wasapi::{initialize_mta, DeviceEnumerator, Direction, StreamMode};

    initialize_mta()
        .ok()
        .map_err(|e| format!("COM init: {e}"))?;

    let enumerator = DeviceEnumerator::new().map_err(|e| format!("enumerator: {e}"))?;
    let device = pick_device(&enumerator, &device_filter)?;
    let label = device
        .get_friendlyname()
        .unwrap_or_else(|_| "?".to_string());
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| format!("audio client: {e}"))?;

    // Read what the endpoint actually produces and use exactly that; the
    // conversion to 44.1k/16/stereo happens in `Converter`.
    let mix = client
        .get_mixformat()
        .map_err(|e| format!("mix format: {e}"))?;
    let rate = mix.get_samplespersec();
    let channels = mix.get_nchannels();
    let bits = mix.get_bitspersample();
    let is_float = matches!(mix.get_subformat(), Ok(wasapi::SampleType::Float));
    log(format!(
        "capturing {label} at {rate} Hz, {bits}-bit, {channels} ch, float={is_float}"
    ));

    let mode = StreamMode::EventsShared {
        autoconvert: false,
        buffer_duration_hns: 200_000,
    };
    client
        .initialize_client(&mix, &Direction::Capture, &mode)
        .map_err(|e| format!("initialize capture: {e}"))?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("event handle: {e}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| format!("capture client: {e}"))?;

    client
        .start_stream()
        .map_err(|e| format!("start stream: {e}"))?;

    let mut conv = Converter::new(rate, channels, bits, is_float);
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut converted: Vec<u8> = Vec::new();
    let max_queue = BYTES_PER_SECOND / 2;

    while alive.load(Ordering::Relaxed) {
        let _ = event.wait_for_event(200);

        loop {
            let before = raw.len();
            match capture.read_from_device_to_deque(&mut raw) {
                Ok(_) => {}
                Err(e) => {
                    log(format!("capture read: {e}"));
                    break;
                }
            }
            if raw.len() == before {
                break;
            }

            let chunk: Vec<u8> = raw.drain(..).collect();
            converted.clear();
            conv.push(&chunk, &mut converted);

            if let Ok(mut q) = queue.lock() {
                q.extend(converted.iter().copied());
                while q.len() > max_queue {
                    q.pop_front();
                }
            } else {
                break;
            }
        }
    }

    let _ = client.stop_stream();
    Ok(())
}
