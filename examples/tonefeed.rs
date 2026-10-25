//! Fills an output endpoint with a quiet sine, so the capture side can be tested
//! without a real player on the other end.
//!
//! This exists for testing the sender end to end on a machine with no AirPlay
//! hardware: render into `CABLE Input`, have the sender capture `CABLE Output` and
//! stream to `tools/mock-receiver.py` on loopback. Nothing is audible unless
//! something else is capturing the cable at the same time, so stop the daemon
//! before running this.
//!
//!     cargo run --release --example tonefeed -- --device "CABLE Input" --seconds 10
//!
//! Not part of the shipped binaries: examples are not built by `cargo build`.

#![cfg(windows)]

use std::f64::consts::PI;
use std::time::{Duration, Instant};
use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode};

/// Pick a render device whose friendly name contains `needle`.
fn pick_render_device(
    enumerator: &DeviceEnumerator,
    needle: &str,
) -> Result<wasapi::Device, String> {
    let lower = needle.to_lowercase();
    let collection = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|e| format!("render devices: {e}"))?;
    let count = collection
        .get_nbr_devices()
        .map_err(|e| format!("render device count: {e}"))?;

    let mut seen = Vec::new();
    for idx in 0..count {
        let device = match collection.get_device_at_index(idx) {
            Ok(d) => d,
            Err(_) => break,
        };
        let name = device.get_friendlyname().unwrap_or_default();
        if name.to_lowercase().contains(&lower) {
            return Ok(device);
        }
        seen.push(name);
    }
    Err(format!(
        "no render device matching {needle:?}; saw: {}",
        seen.join(", ")
    ))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let needle = get("--device").unwrap_or_else(|| "CABLE Input".to_string());
    let seconds: f64 = get("--seconds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10.0);
    let hertz: f64 = get("--hz").and_then(|v| v.parse().ok()).unwrap_or(440.0);
    let amplitude: f32 = get("--amplitude")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.2);

    initialize_mta()
        .ok()
        .map_err(|e| format!("COM init: {e}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|e| format!("enumerator: {e}"))?;
    let device = pick_render_device(&enumerator, &needle)?;
    let label = device
        .get_friendlyname()
        .unwrap_or_else(|_| "?".to_string());

    let mut client = device
        .get_iaudioclient()
        .map_err(|e| format!("audio client: {e}"))?;

    // Use the endpoint's own mix format: whatever it renders in shared mode is
    // exactly what the capture side will see.
    let mix = client
        .get_mixformat()
        .map_err(|e| format!("mix format: {e}"))?;
    let rate = mix.get_samplespersec();
    let channels = mix.get_nchannels();
    let bits = mix.get_bitspersample();
    let blockalign = mix.get_blockalign() as usize;
    let is_float = matches!(mix.get_subformat(), Ok(SampleType::Float));
    let supported = (is_float && bits == 32) || (!is_float && bits == 16);
    if !supported {
        return Err(format!(
            "unsupported endpoint format: {rate} Hz, {bits}-bit, {channels} ch, float={is_float}"
        ));
    }

    let (def_time, _min_time) = client
        .get_device_period()
        .map_err(|e| format!("device period: {e}"))?;
    let mode = StreamMode::EventsShared {
        autoconvert: false,
        buffer_duration_hns: def_time,
    };
    client
        .initialize_client(&mix, &Direction::Render, &mode)
        .map_err(|e| format!("initialize render: {e}"))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("event handle: {e}"))?;
    let render = client
        .get_audiorenderclient()
        .map_err(|e| format!("render client: {e}"))?;

    println!(
        "tonefeed: {label} at {rate} Hz, {bits}-bit, {channels} ch, float={is_float}, \
         {amplitude} amplitude for {seconds}s"
    );

    let channels = channels as usize;
    let mut phase = 0.0f64;
    let delta = 2.0 * PI * hertz / rate as f64;

    let mut write = |frames: usize| -> Result<(), String> {
        let mut data = vec![0u8; frames * blockalign];
        for frame in data.chunks_exact_mut(blockalign) {
            let sample = (phase.sin() as f32) * amplitude;
            phase += delta;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
            let bytes = if is_float {
                sample.to_le_bytes().to_vec()
            } else {
                ((sample * 32767.0) as i16).to_le_bytes().to_vec()
            };
            for channel in frame.chunks_exact_mut(blockalign / channels) {
                channel.copy_from_slice(&bytes[..channel.len().min(bytes.len())]);
            }
        }
        render
            .write_to_device(frames, &data, None)
            .map_err(|e| format!("write: {e}"))
    };

    // Prime the buffer so playback starts with signal rather than silence.
    let space = client
        .get_available_space_in_frames()
        .map_err(|e| format!("space: {e}"))?;
    write(space as usize)?;
    client
        .start_stream()
        .map_err(|e| format!("start stream: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        if event.wait_for_event(500).is_err() {
            break;
        }
        let space = client
            .get_available_space_in_frames()
            .map_err(|e| format!("space: {e}"))?;
        if space > 0 {
            write(space as usize)?;
        }
    }

    let _ = client.stop_stream();
    println!("tonefeed: done");
    Ok(())
}
