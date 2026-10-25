//! Invisible background sender.
//!
//! Captures a virtual audio cable and streams it to an AirPlay 1 receiver. No
//! window, no console, no tray icon: it is meant to be started at logon by a
//! scheduled task.
//!
//! Configuration is read from the process environment, with a
//! `raop-sender.env` file next to the executable as a fallback, so a scheduled
//! task picks up the right values without needing a fresh logon:
//!
//! ```text
//! RAOP_HOST=192.0.2.10:5000
//! RAOP_DEVICE=CABLE
//! RAOP_PRIME_MS=60
//! RAOP_BUFFER_MS=60
//! ```

#![windows_subsystem = "windows"]

use raop_sender::session::{self, Settings, Source};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Safety net only. The session normally rotates itself when the stream goes
/// idle (`RAOP_IDLE_MS`, 2 minutes), so audio is not cut mid-track. This timer
/// exists for the case where audio keeps arriving but the receiver has fallen
/// asleep, which cannot be distinguished from the sending side yet.
const SESSION_MINUTES: u64 = 60;

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn log_path() -> PathBuf {
    exe_dir().join("daemon.log")
}

fn trace(line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{stamp}] {line}");
    }
}

/// Read `raop-sender.env` next to the executable and export it, without
/// overriding anything already present in the real environment.
fn load_env_file() {
    let path = exe_dir().join("raop-sender.env");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut applied = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || std::env::var_os(key).is_some() {
                continue;
            }
            std::env::set_var(key, value);
            applied += 1;
        }
    }
    if applied > 0 {
        trace(&format!("loaded {applied} setting(s) from raop-sender.env"));
    }
}

fn main() {
    trace("daemon starting");
    load_env_file();

    // No receiver address is compiled in: it is deployment-specific.
    let host = std::env::var("RAOP_HOST").unwrap_or_default();
    if host.is_empty() {
        trace("RAOP_HOST is not set; write it to raop-sender.env next to this exe");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    let device = std::env::var("RAOP_DEVICE").unwrap_or_else(|_| "CABLE".to_string());
    trace(&format!(
        "receiver {host}, capture device filter {device:?}"
    ));

    let log: session::Log = Arc::new(|line: String| trace(&line));

    loop {
        let cfg = Settings {
            host: host.clone(),
            // Volume writes are disabled: this is the receiver's master level and
            // is shared with its TV/HDMI output.
            volume_db: 0.0,
            set_volume: false,
            amplitude: 0.0,
            seconds: 0.0,
            tone_hz: 440.0,
            source: Source::System,
            device_filter: Some(device.clone()),
            probe_only: false,
        };

        let stop = Arc::new(AtomicBool::new(false));

        let watchdog = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(SESSION_MINUTES * 60));
            watchdog.store(true, Ordering::Relaxed);
        });

        if let Err(e) = session::play(&cfg, log.clone(), stop.clone()) {
            trace(&format!("session error: {e}"));
        }
        stop.store(true, Ordering::Relaxed);

        std::thread::sleep(Duration::from_secs(3));
    }
}
