//! WASAPI capture of the virtual cable.
//!
//! This module only moves bytes: it reads the endpoint's real mix format, logs it,
//! and hands the raw bytes to `crate::pcm::Converter`. The conversion itself lives
//! there because it is platform-neutral and testable; a wrong sample type is what
//! produced loud static, and that code deserves tests.

#![cfg(windows)]

use crate::pcm::{Converter, BYTES_PER_SECOND};
use crate::session::Log;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
