//! `--doctor`: check every precondition a silent stream depends on.
//!
//! Every failure this project has hit in the field was silent: the receiver
//! answered `200 OK` and no audio was produced. There were three causes — a
//! missing inbound firewall rule, nothing routed into the cable, and writing the
//! receiver's shared volume. This command exists so the next person finds them in
//! one run instead of by hand.
//!
//! It never plays audio: it reads the cable locally and asks the receiver for its
//! `/info` document, which opens no session.

use crate::config;
use crate::rtsp::RtspClient;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Unknown,
}

impl Status {
    fn glyph(self) -> &'static str {
        match self {
            Status::Pass => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Unknown => "?   ",
        }
    }
}

pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    /// What the reader should do about it, shown only when not passing.
    pub fix: Option<&'static str>,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: &'static str) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: &'static str) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix),
        }
    }

    fn unknown(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Unknown,
            detail: detail.into(),
            fix: None,
        }
    }
}

pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn failed(&self) -> bool {
        self.checks.iter().any(|c| c.status == Status::Fail)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            out.push_str(&format!(
                "  [{}] {:<14} {}\n",
                check.status.glyph(),
                check.name,
                check.detail
            ));
            if let Some(fix) = check.fix {
                if check.status != Status::Pass {
                    out.push_str(&format!("         {}  ->  {fix}\n", " ".repeat(14)));
                }
            }
        }
        out
    }
}

/// Peak the session needs before it starts streaming; keep in sync with
/// `session::stream_system`.
const AUDIO_GATE: u16 = 200;

pub fn run(host: Option<&str>, capture_seconds: f64) -> Report {
    let mut checks = Vec::new();

    checks.push(config_check(host));

    if let Some(host) = host.filter(|h| h.contains(':')) {
        checks.push(receiver_tcp(host));
        checks.push(receiver_info(host));
    } else {
        checks.push(Check::unknown("receiver tcp", "skipped, no address"));
        checks.push(Check::unknown("receiver /info", "skipped, no address"));
    }

    checks.extend(capture_checks(capture_seconds));
    checks.push(firewall());
    checks.push(task());

    Report { checks }
}

/// Configuration only, no I/O. Pure so the tests can exercise it.
pub fn config_check(host: Option<&str>) -> Check {
    match host {
        Some(h) if h.contains(':') => {
            let mut detail = format!("RAOP_HOST={h}, RAOP_DEVICE={}", config::device_filter());
            if config::env_file().is_file() {
                detail.push_str(", raop-sender.env present");
            }
            Check::pass("config", detail)
        }
        Some(h) => Check::fail(
            "config",
            format!("RAOP_HOST={h} has no port"),
            "append the RAOP service port, normally :5000",
        ),
        None => Check::fail(
            "config",
            "RAOP_HOST is not set",
            "write RAOP_HOST=<ip>:5000 to raop-sender.env next to the exe",
        ),
    }
}

fn receiver_tcp(host: &str) -> Check {
    let addr = match host.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => {
            return Check::fail(
                "receiver tcp",
                format!("{host} does not resolve"),
                "check the address in RAOP_HOST",
            )
        }
    };
    match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
        Ok(_) => Check::pass("receiver tcp", format!("{addr} accepts connections")),
        Err(e) => Check::fail(
            "receiver tcp",
            format!("{addr}: {e}"),
            "the receiver is off, asleep, or on another network",
        ),
    }
}

fn receiver_info(host: &str) -> Check {
    let mut client = match RtspClient::connect(host, Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => {
            return Check::fail(
                "receiver /info",
                format!("connect failed: {e}"),
                "see the receiver tcp check",
            )
        }
    };
    match client.request("GET", "/info", &[], &[]) {
        Ok(r) if r.is_ok() => {
            let body = String::from_utf8_lossy(&r.body);
            // A plist document: the key and its value are usually on separate
            // lines, so search forward from the key rather than within its line.
            let model = body.find("<key>model</key>").and_then(|at| {
                let rest = &body[at..];
                let open = rest.find("<string>")? + "<string>".len();
                let close = rest[open..].find("</string>")? + open;
                Some(rest[open..close].to_string())
            });
            let model = model.unwrap_or_else(|| "model not published".to_string());
            Check::pass(
                "receiver /info",
                format!("{} {} ({model})", r.code, r.reason),
            )
        }
        Ok(r) => Check::warn(
            "receiver /info",
            format!("{} {}", r.code, r.reason),
            "not fatal; some receivers do not publish /info",
        ),
        Err(e) => Check::fail(
            "receiver /info",
            format!("{e}"),
            "check that the port in RAOP_HOST is the RAOP service",
        ),
    }
}

#[cfg(windows)]
fn capture_checks(seconds: f64) -> Vec<Check> {
    let filter = Some(config::device_filter());
    match crate::capture::probe_level(filter, Duration::from_secs_f64(seconds.clamp(0.2, 5.0))) {
        Ok(probe) => {
            let format = format!(
                "{} at {} Hz, {}-bit, {} ch, float={}, {} frames heard",
                probe.device, probe.rate, probe.bits, probe.channels, probe.is_float, probe.frames
            );
            let mut out = vec![Check::pass("capture device", format)];

            let dbfs = if probe.peak == 0 {
                "-inf".to_string()
            } else {
                format!("{:.1}", 20.0 * (probe.peak as f64 / 32_768.0).log10())
            };
            if probe.peak == 0 {
                out.push(Check::fail(
                    "audio level",
                    format!("peak 0 ({dbfs} dBFS): nothing is being routed here"),
                    "set the Windows output device to CABLE Input, then play something",
                ));
            } else if probe.peak <= AUDIO_GATE {
                out.push(Check::warn(
                    "audio level",
                    format!(
                        "peak {} ({dbfs} dBFS) is at or below the {AUDIO_GATE} start gate",
                        probe.peak
                    ),
                    "the sender stays silent until the level rises; play something louder",
                ));
            } else {
                out.push(Check::pass(
                    "audio level",
                    format!(
                        "peak {} ({dbfs} dBFS), above the {AUDIO_GATE} start gate",
                        probe.peak
                    ),
                ));
            }
            out
        }
        Err(e) => {
            let mut out = vec![Check::fail(
                "capture device",
                e,
                "install VB-CABLE, or point RAOP_DEVICE at an existing capture device",
            )];
            out.push(Check::unknown("audio level", "skipped, no capture device"));
            out
        }
    }
}

#[cfg(not(windows))]
fn capture_checks(_seconds: f64) -> Vec<Check> {
    vec![
        Check::unknown("capture device", "system capture is Windows-only"),
        Check::unknown("audio level", "system capture is Windows-only"),
    ]
}

/// The receiver sends timing requests back to the sender, so the daemon needs an
/// inbound UDP rule scoped to its own executable. Without it every request still
/// returns 200 and nothing plays.
#[cfg(windows)]
fn firewall() -> Check {
    let rule = config::task_name();
    let command = format!(
        "(Get-NetFirewallRule -DisplayName '{rule}' -ErrorAction SilentlyContinue | Measure-Object).Count"
    );
    match powershell(&command) {
        Ok(out) => {
            let count: usize = out.trim().parse().unwrap_or(0);
            if count > 0 {
                Check::pass("firewall", format!("rule '{rule}' is present"))
            } else {
                Check::fail(
                    "firewall",
                    format!("no inbound rule named '{rule}'"),
                    "run install.ps1 as administrator, or Add-NetFirewallRule for the daemon",
                )
            }
        }
        Err(e) => Check::unknown("firewall", format!("could not query: {e}")),
    }
}

#[cfg(not(windows))]
fn firewall() -> Check {
    Check::unknown("firewall", "Windows-only")
}

#[cfg(windows)]
fn task() -> Check {
    let name = config::task_name();
    let command = format!(
        "(Get-ScheduledTask -TaskName '{name}' -ErrorAction SilentlyContinue | Measure-Object).Count"
    );
    match powershell(&command) {
        Ok(out) => {
            let count: usize = out.trim().parse().unwrap_or(0);
            if count > 0 {
                Check::pass("scheduled task", format!("'{name}' is registered"))
            } else {
                Check::warn(
                    "scheduled task",
                    format!("'{name}' is not registered"),
                    "run install.ps1 with -Receiver to start the daemon at logon",
                )
            }
        }
        Err(e) => Check::unknown("scheduled task", format!("could not query: {e}")),
    }
}

#[cfg(not(windows))]
fn task() -> Check {
    Check::unknown("scheduled task", "Windows-only")
}

#[cfg(windows)]
fn powershell(command: &str) -> Result<String, String> {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_host_fails_the_config_check() {
        let check = config_check(None);
        assert_eq!(check.name, "config");
        assert_eq!(check.status, Status::Fail);
        assert!(check.fix.is_some(), "a failure must say what to do");
    }

    #[test]
    fn a_host_without_a_port_is_rejected() {
        let check = config_check(Some("192.0.2.10"));
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("no port"));
    }

    #[test]
    fn a_complete_address_passes() {
        let check = config_check(Some("192.0.2.10:5000"));
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains("192.0.2.10:5000"));
    }

    #[test]
    fn rendering_never_hides_a_failing_check() {
        let report = Report {
            checks: vec![
                Check::pass("a", "fine"),
                Check::fail("b", "broken", "do the thing"),
            ],
        };
        let text = report.render();
        assert!(text.contains("[FAIL]"));
        assert!(text.contains("do the thing"));
        assert!(report.failed());
    }

    #[test]
    fn a_passing_check_does_not_print_its_fix() {
        let report = Report {
            checks: vec![Check {
                name: "a",
                status: Status::Pass,
                detail: "fine".into(),
                fix: Some("unused"),
            }],
        };
        assert!(!report.render().contains("unused"));
    }
}
