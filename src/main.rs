//! Command-line debug front end for the RAOP sender.
//!
//! Audio is never played implicitly: `--play` is required.

use raop_sender::session::{self, Settings};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    let probe = args.iter().any(|a| a == "--probe-volume");
    if !args.iter().any(|a| a == "--play") && !probe {
        eprintln!("refusing to play: pass --play (audio is always explicit)");
        eprintln!();
        eprintln!("  raop-sender --play [--seconds N] [--volume-db X] [--host IP:PORT]");
        eprintln!("  raop-sender --probe-volume     (handshake + volume only, silent)");
        return ExitCode::FAILURE;
    }

    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let mut cfg = Settings {
        probe_only: probe,
        ..Settings::default()
    };
    if let Some(v) = get("--seconds").and_then(|v| v.parse().ok()) {
        cfg.seconds = v;
    }
    if let Some(v) = get("--volume-db").and_then(|v| v.parse().ok()) {
        cfg.volume_db = v;
        cfg.amplitude = 0.25;
    }
    cfg.host = get("--host")
        .or_else(|| std::env::var("RAOP_HOST").ok())
        .unwrap_or_default();
    if cfg.host.is_empty() {
        eprintln!("error: no receiver configured");
        eprintln!("  pass --host IP:PORT, or set RAOP_HOST");
        return ExitCode::FAILURE;
    }

    let log: session::Log = Arc::new(|line: String| println!("{line}"));
    let stop = Arc::new(AtomicBool::new(false));

    println!("volume {:.1} dB, {:.1} s", cfg.volume_db, cfg.seconds);
    match session::play(&cfg, log, stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
