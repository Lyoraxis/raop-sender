//! Small GUI front end: pick a volume, press Play.
//!
//! Nothing is played on start-up. Audio only happens when the Play button is
//! pressed, and it begins at the volume shown on the slider.

use eframe::egui;
use raop_sender::session::{self, Settings, Source};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Every run is mirrored here so a failure can be read back off disk.
fn log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .map(|d| d.join("last-run.log"))
        .unwrap_or_else(|| PathBuf::from("last-run.log"))
}

fn default_host() -> String {
    std::env::var("RAOP_HOST").unwrap_or_default()
}

/// Fixed signal level. Loudness is the receiver volume slider, not this.
const TONE_AMPLITUDE: f64 = 0.25;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([540.0, 500.0])
            .with_min_inner_size([440.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Devialet AirPlay",
        options,
        Box::new(|_cc| Box::new(App::new())),
    )
}

struct App {
    host: String,
    volume_db: f64,
    seconds: f64,
    system_audio: bool,
    playing: bool,
    stopping: bool,
    stop: Arc<AtomicBool>,
    rx: Option<Receiver<String>>,
    log: String,
    handle: Option<JoinHandle<()>>,
}

impl App {
    fn new() -> Self {
        Self {
            host: default_host().to_string(),
            volume_db: -18.0,
            seconds: 6.0,
            system_audio: false,
            playing: false,
            stopping: false,
            stop: Arc::new(AtomicBool::new(false)),
            rx: None,
            log: String::new(),
            handle: None,
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.log.clear();
        self.playing = true;
        self.stopping = false;

        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();

        let cfg = Settings {
            host: self.host.clone(),
            volume_db: self.volume_db,
            set_volume: true,
            amplitude: TONE_AMPLITUDE,
            seconds: self.seconds,
            tone_hz: 440.0,
            device_filter: if self.system_audio {
                Some("CABLE".to_string())
            } else {
                None
            },
            source: if self.system_audio {
                Source::System
            } else {
                Source::Tone
            },
            probe_only: false,
        };

        let path = log_path();
        // Append, so several attempts in one session can be compared.
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(
                f,
                "\n=== run: volume {:.1} dB, amplitude {:.2}, {:.0} s ===",
                self.volume_db, TONE_AMPLITUDE, self.seconds
            );
        }
        let log: session::Log = Arc::new(move |line: String| {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{line}");
            }
            let _ = tx.send(line);
        });

        let ctx = ctx.clone();
        self.handle = Some(std::thread::spawn(move || {
            let _ = session::play(&cfg, log, stop);
            ctx.request_repaint();
        }));
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.rx {
            while let Ok(line) = rx.try_recv() {
                self.log.push_str(&line);
                self.log.push('\n');
            }
        }
        let finished = self
            .handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false);
        if finished && self.playing {
            self.playing = false;
            self.stopping = false;
            self.handle = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(120));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Devialet AirPlay");
            ui.add_enabled(
                !self.playing,
                egui::TextEdit::singleline(&mut self.host)
                    .hint_text("receiver IP:PORT (or set RAOP_HOST)")
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label("source");
                ui.add_enabled_ui(!self.playing, |ui| {
                    ui.radio_value(&mut self.system_audio, false, "test tone");
                    ui.radio_value(&mut self.system_audio, true, "system audio");
                });
            });
            ui.add_space(4.0);

            ui.add(
                egui::Slider::new(&mut self.volume_db, -26.0..=0.0)
                    .text("volume")
                    .suffix(" dB")
                    .fixed_decimals(1),
            );
            ui.add_enabled(
                !self.system_audio,
                egui::Slider::new(&mut self.seconds, 1.0..=20.0)
                    .text("duration")
                    .suffix(" s")
                    .fixed_decimals(0),
            );

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("presets").small().weak());
                if ui.small_button("quiet −24").clicked() {
                    self.volume_db = -24.0;
                }
                if ui.small_button("normal −12").clicked() {
                    self.volume_db = -12.0;
                }
                if ui.small_button("loud −4").clicked() {
                    self.volume_db = -4.0;
                }
            });

            ui.add_space(4.0);
            // Measured: this Player mutes entirely below about -28 dB, so the
            // slider stops at -26 and the floor is called out instead of being
            // a silent trap.
            let hint = if self.volume_db <= -24.0 {
                "Quiet — just above the Player's mute point (−28)."
            } else if self.volume_db <= -10.0 {
                "Moderate level."
            } else {
                "Loud — close to full output."
            };
            ui.label(egui::RichText::new(hint).small().weak());

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.playing, egui::Button::new("▶  Play"))
                    .clicked()
                {
                    self.start(ctx);
                }
                if ui
                    .add_enabled(self.playing && !self.stopping, egui::Button::new("■  Stop"))
                    .clicked()
                {
                    self.stopping = true;
                    self.stop.store(true, Ordering::Relaxed);
                }
                if self.playing {
                    ui.spinner();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("log").small().weak());

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.monospace(&self.log);
                });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
