//! Command-line debug front end for the RAOP sender.
//!
//! Audio is never played implicitly: `--play` is required. `--doctor` and
//! `--probe-volume` diagnose without producing sound.

use raop_sender::config;
use raop_sender::doctor;
use raop_sender::session::{self, Settings, Source};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const USAGE: &str = "\
  raop-sender --play [--seconds N] [--volume-db X] [--host IP:PORT] [--system]
  raop-sender --doctor [--host IP:PORT]   check every precondition, plays nothing
  raop-sender --probe-volume              handshake + volume only, silent";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    let doctor_mode = args.iter().any(|a| a == "--doctor");
    let system = args.iter().any(|a| a == "--system");
    let probe = args.iter().any(|a| a == "--probe-volume");
    let play = args.iter().any(|a| a == "--play");

    if doctor_mode {
        // The same fallback file the daemon reads, so the report describes the
        // configuration that will actually run.
        let applied = config::load_env_file();
        let host = args
            .iter()
            .position(|a| a == "--host")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .or_else(config::host);
        if applied > 0 {
            println!("loaded {applied} setting(s) from raop-sender.env\n");
        }
        println!("raop-sender doctor\n");
        let report = doctor::run(host.as_deref(), 1.0);
        print!("{}", report.render());
        println!();
        if report.failed() {
            println!("result: at least one check failed, fix the FAIL lines above");
            return ExitCode::FAILURE;
        }
        println!("result: every precondition a stream depends on is satisfied");
        return ExitCode::SUCCESS;
    }

    if !play && !probe {
        eprintln!("refusing to play: pass --play (audio is always explicit)");
        eprintln!();
        eprintln!("{USAGE}");
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
    if system {
        // Capture the cable instead of generating a tone: this is what the daemon
        // does, exposed so the same path can be tested from a terminal.
        cfg.source = Source::System;
        cfg.device_filter = Some(config::device_filter());
        cfg.set_volume = false;
    }
    if let Some(v) = get("--seconds").and_then(|v| v.parse().ok()) {
        cfg.seconds = v;
    }
    if let Some(v) = get("--volume-db").and_then(|v| v.parse().ok()) {
        cfg.volume_db = v;
        cfg.amplitude = 0.25;
    }
    cfg.host = get("--host").or_else(config::host).unwrap_or_default();
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
