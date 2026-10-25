//! RAOP session: handshake plus real-time ALAC streaming.
//!
//! Everything here is driven by the caller. Nothing plays on construction —
//! `play()` is only entered when a human asks for it.

use crate::crypto;
use crate::rtsp::{Response, RtspClient};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SAMPLE_RATE: u32 = 44100;
const FRAMES_PER_PACKET: u32 = 352;
const TIMEOUT: Duration = Duration::from_secs(15);

/// How far ahead of real time we push audio. The receiver plays as soon as it
/// has data, so sitting exactly on the wall clock turns any scheduling jitter
/// into a gap — which is what makes a clean tone sound buzzy.
const LEAD_SECS: f64 = 0.0;

static RTP_NOW: AtomicU32 = AtomicU32::new(0);
static TIMING_REQUESTS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Generated sine test tone for `seconds`.
    Tone,
    /// Live system audio from the default render endpoint, until stopped.
    System,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub host: String,
    /// Device volume in dB (`SET_PARAMETER volume`).
    pub volume_db: f64,
    /// Whether to write the receiver's volume at all. The Player's volume is a
    /// single master level shared with its TV/HDMI output, so a background
    /// sender must never touch it.
    pub set_volume: bool,
    /// Tone amplitude, 0.0 .. 1.0.
    pub amplitude: f64,
    pub seconds: f64,
    pub tone_hz: f64,
    pub source: Source,
    /// Capture device name substring; `None` means loopback the default output.
    pub device_filter: Option<String>,
    /// Handshake and volume only; no RECORD, no RTP, no audio.
    pub probe_only: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: String::new(),
            volume_db: -12.0,
            set_volume: true,
            amplitude: 0.25,
            seconds: 5.0,
            tone_hz: 440.0,
            source: Source::Tone,
            device_filter: None,
            probe_only: false,
        }
    }
}

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

fn seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1
}

fn next(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn hex(value: u64, width: usize) -> String {
    format!("{value:0width$X}")
}

fn local_ip(remote: &str) -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(remote)?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "0.0.0.0".to_string())
}

fn ntp_now() -> (u32, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total = d.as_secs_f64() + 2_208_988_800.0;
    let secs = total as u64;
    let frac = ((total - total.trunc()) * (1u64 << 32) as f64) as u64;
    (secs as u32, frac as u32)
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn build_sdp(session: u32, local: &str, remote: &str, rsaaeskey: &str, aesiv: &str) -> String {
    let mut sdp = String::new();
    sdp.push_str("v=0\r\n");
    sdp.push_str(&format!("o=iTunes {session} 0 IN IP4 {local}\r\n"));
    sdp.push_str("s=iTunes\r\n");
    sdp.push_str(&format!("c=IN IP4 {remote}\r\n"));
    sdp.push_str("t=0 0\r\n");
    sdp.push_str("m=audio 0 RTP/AVP 96\r\n");
    sdp.push_str("a=rtpmap:96 AppleLossless\r\n");
    sdp.push_str(&format!("a=fmtp:96 {}\r\n", crate::alac::FMTP));
    sdp.push_str(&format!("a=rsaaeskey:{rsaaeskey}\r\n"));
    sdp.push_str(&format!("a=aesiv:{aesiv}\r\n"));
    sdp
}

fn transport_param(transport: &str, key: &str) -> Option<u16> {
    transport.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .and_then(|v| v.parse().ok())
    })
}

fn report(log: &Log, step: &str, resp: &Response) {
    log(format!("{step:<20} {} {}", resp.code, resp.reason));
    for (k, v) in &resp.headers {
        log(format!("    {k}: {v}"));
    }
}

fn spawn_sync_sender(remote_ip: String, control_port: u16, alive: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let dest = format!("{remote_ip}:{control_port}");
        let sock = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut first = true;
        while alive.load(Ordering::Relaxed) {
            let (sec, frac) = ntp_now();
            let rtp = RTP_NOW.load(Ordering::Relaxed);
            let mut pkt = Vec::with_capacity(20);
            pkt.push(if first { 0x90 } else { 0x80 });
            pkt.push(0xD4);
            pkt.extend_from_slice(&7u16.to_be_bytes());
            pkt.extend_from_slice(&rtp.to_be_bytes());
            pkt.extend_from_slice(&sec.to_be_bytes());
            pkt.extend_from_slice(&frac.to_be_bytes());
            pkt.extend_from_slice(&rtp.to_be_bytes());
            let _ = sock.send_to(&pkt, &dest);
            first = false;
            std::thread::sleep(Duration::from_millis(250));
        }
    });
}

fn spawn_timing_server(sock: UdpSocket, alive: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        sock.set_read_timeout(Some(Duration::from_millis(500))).ok();
        let mut buf = [0u8; 256];
        while alive.load(Ordering::Relaxed) {
            let (n, src) = match sock.recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if n < 32 {
                continue;
            }
            TIMING_REQUESTS.fetch_add(1, Ordering::Relaxed);
            let mut reply = [0u8; 32];
            reply.copy_from_slice(&buf[..32]);
            reply[1] = 0xD3;
            let (sec, frac) = ntp_now();
            reply[16..20].copy_from_slice(&sec.to_be_bytes());
            reply[20..24].copy_from_slice(&frac.to_be_bytes());
            reply[24..28].copy_from_slice(&sec.to_be_bytes());
            reply[28..32].copy_from_slice(&frac.to_be_bytes());
            let _ = sock.send_to(&reply, src);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn audio_packet(
    alac: &mut crate::alac::Alac,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    frames: u32,
    tone_hz: f64,
    start_frame: u32,
    amplitude: f64,
    key: &[u8; 16],
    iv: &[u8; 16],
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 * frames as usize);
    for i in 0..frames {
        let frame = start_frame + i;
        let t = frame as f64 / SAMPLE_RATE as f64;
        let sample =
            ((2.0 * std::f64::consts::PI * tone_hz * t).sin() * amplitude * 32767.0) as i16;
        let bytes = sample.to_le_bytes();
        payload.extend_from_slice(&bytes);
        payload.extend_from_slice(&bytes);
    }

    let frame = alac.encode(&payload).to_vec();
    let cipher = crypto::encrypt_payload(&frame, key, iv);

    let mut packet = Vec::with_capacity(12 + cipher.len());
    packet.push(0x80);
    packet.push(0x60);
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(&cipher);
    packet
}

fn pace(start: std::time::Instant, frames_sent: u32) {
    let target = Duration::from_secs_f64(frames_sent as f64 / SAMPLE_RATE as f64 - LEAD_SECS);
    loop {
        let elapsed = start.elapsed();
        if elapsed >= target {
            break;
        }
        let remain = target - elapsed;
        if remain > Duration::from_micros(800) {
            std::thread::sleep(remain - Duration::from_micros(800));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Run one complete session. Returns when the tone is finished, the caller
/// asked to stop, or the receiver refuses.
pub fn play(cfg: &Settings, log: Log, stop: Arc<AtomicBool>) -> Result<(), String> {
    let alive = Arc::new(AtomicBool::new(true));
    let result = play_inner(cfg, &log, &stop, &alive);
    alive.store(false, Ordering::Relaxed);
    result
}

/// Build a packet from little-endian 16-bit stereo PCM (what WASAPI gives us),
/// encoding it as ALAC before encryption.
#[cfg(windows)]
fn audio_packet_bytes(
    alac: &mut crate::alac::Alac,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    pcm_le: &[u8],
    key: &[u8; 16],
    iv: &[u8; 16],
) -> Vec<u8> {
    let frame = alac.encode(pcm_le).to_vec();
    let cipher = crypto::encrypt_payload(&frame, key, iv);
    let mut packet = Vec::with_capacity(12 + cipher.len());
    packet.push(0x80);
    packet.push(0x60);
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(&cipher);
    packet
}

/// How long the stream may carry nothing before the session is torn down.
///
/// Silence does not idle the session: a silent packet every 8 ms keeps the RTP
/// timeline alive, so a short pause costs nothing on resume. This timer only
/// exists to re-arm a receiver that fell asleep mid-session, and it is
/// deliberately long for that reason — rotating too eagerly turned every pause
/// in playback into a reconnect the listener could hear.
#[cfg(windows)]
fn idle_rotate_ms() -> usize {
    env_ms("RAOP_IDLE_MS", 600_000)
}

/// Milliseconds from an environment variable, if set and sane.
#[cfg(windows)]
fn env_ms(name: &str, default: u64) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v <= 5_000)
        .unwrap_or(default) as usize
}

/// Stream live system audio until `stop` is set.
#[allow(clippy::too_many_arguments)]
fn stream_system(
    cfg: &Settings,
    log: &Log,
    stop: &Arc<AtomicBool>,
    alive: &Arc<AtomicBool>,
    sock: &UdpSocket,
    ssrc: u32,
    key: &[u8; 16],
    iv: &[u8; 16],
) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (cfg, log, stop, alive, sock, ssrc, key, iv);
        Err("system audio capture is Windows-only".to_string())
    }

    #[cfg(windows)]
    {
        let _ = cfg;
        use std::collections::VecDeque;
        use std::sync::Mutex;

        let queue = Arc::new(Mutex::new(VecDeque::<u8>::new()));
        crate::capture::start(
            log.clone(),
            alive.clone(),
            queue.clone(),
            cfg.device_filter.clone(),
        )?;

        // Prefill half a second so the first packets are not silence.
        let prime_bytes = crate::capture::BYTES_PER_SECOND * env_ms("RAOP_PRIME_MS", 60) / 1000;
        let want = prime_bytes.max(crate::capture::BYTES_PER_SECOND / 100);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let have = queue.lock().map(|q| q.len()).unwrap_or(0);
            if have >= want || std::time::Instant::now() > deadline {
                if have == 0 {
                    log(
                        "no audio from the capture device yet - is media routed to it?".to_string(),
                    );
                } else {
                    log(format!(
                        "buffer primed with {} ms",
                        have * 1000 / crate::capture::BYTES_PER_SECOND
                    ));
                }
                break;
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut alac = crate::alac::Alac::new();
        let mut seq: u16 = 0;
        let mut timestamp: u32 = 60_000;
        let mut start = std::time::Instant::now();
        let mut sent: u32 = 0;
        let mut starved: u64 = 0;
        let mut started = false;

        // Stay silent until there is something to play. Starting a RTP stream
        // of digital zeros makes the receiver emit an audible transient, which
        // is why pressing play with nothing routed to the cable still made
        // noise.
        while !started && !stop.load(Ordering::Relaxed) && alive.load(Ordering::Relaxed) {
            let mut probe = vec![0u8; FRAMES_PER_PACKET as usize * 4];
            let mut got = 0usize;
            if let Ok(mut q) = queue.lock() {
                while got < probe.len() {
                    match q.pop_front() {
                        Some(b) => {
                            probe[got] = b;
                            got += 1;
                        }
                        None => break,
                    }
                }
            }
            let peak = probe[..got]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                .max()
                .unwrap_or(0);
            if peak > 200 {
                started = true;
                start = std::time::Instant::now();
                // Drop everything older than the last quarter second: the queue
                // grows while we wait for audio, and playing it from the front
                // is what made playback seconds late.
                let keep = (crate::capture::BYTES_PER_SECOND * env_ms("RAOP_BUFFER_MS", 60) / 1000)
                    .max(crate::capture::BYTES_PER_SECOND / 100);
                let mut dropped = 0usize;
                if let Ok(mut q) = queue.lock() {
                    while q.len() > keep {
                        q.pop_front();
                        dropped += 1;
                    }
                    for b in probe[..got].iter().rev() {
                        q.push_front(*b);
                    }
                }
                log(format!(
                    "audio detected (peak {peak}), starting stream (dropped {} ms of backlog)",
                    dropped * 1000 / crate::capture::BYTES_PER_SECOND
                ));
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let idle_limit = Duration::from_millis(idle_rotate_ms() as u64);
        let mut last_sound = std::time::Instant::now();

        while started && !stop.load(Ordering::Relaxed) && alive.load(Ordering::Relaxed) {
            if last_sound.elapsed() > idle_limit {
                log(format!(
                    "no audio for {} s, rotating the session",
                    idle_limit.as_secs()
                ));
                break;
            }

            let mut chunk = vec![0u8; FRAMES_PER_PACKET as usize * 4];
            let mut got = 0usize;
            if let Ok(mut q) = queue.lock() {
                while got < chunk.len() {
                    match q.pop_front() {
                        Some(b) => {
                            chunk[got] = b;
                            got += 1;
                        }
                        None => break,
                    }
                }
            }
            if got < chunk.len() {
                starved += 1;
                for b in chunk[got..].iter_mut() {
                    *b = 0;
                }
            } else {
                let peak = chunk
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                    .max()
                    .unwrap_or(0);
                if peak > 200 {
                    last_sound = std::time::Instant::now();
                }
            }

            let packet = audio_packet_bytes(&mut alac, seq, timestamp, ssrc, &chunk, key, iv);
            sock.send(&packet).map_err(|e| format!("send: {e}"))?;
            RTP_NOW.store(timestamp, Ordering::Relaxed);

            seq = seq.wrapping_add(1);
            timestamp = timestamp.wrapping_add(FRAMES_PER_PACKET);
            sent += FRAMES_PER_PACKET;
            if seq % 500 == 0 {
                // Exact sender-side pipeline delay: audio sitting in our queue
                // has not been transmitted yet, so it is pure added latency.
                let queued = queue.lock().map(|q| q.len()).unwrap_or(0);
                let queued_ms = queued * 1000 / crate::capture::BYTES_PER_SECOND;
                log(format!(
                    "  {} packets, {} underruns, pipeline delay {} ms",
                    seq as u32, starved, queued_ms
                ));
            }
            pace(start, sent);
        }

        log(format!(
            "capture session ended: {} packets, {} underruns",
            seq as u32, starved
        ));
        Ok(())
    }
}

fn play_inner(
    cfg: &Settings,
    log: &Log,
    stop: &Arc<AtomicBool>,
    alive: &Arc<AtomicBool>,
) -> Result<(), String> {
    let host = cfg.host.clone();
    let remote_ip = host.split(':').next().unwrap_or("127.0.0.1").to_string();
    let local = std::env::var("RAOP_LOCAL_IP").unwrap_or_else(|_| local_ip(&host));

    let mut rng = seed();
    let session: u32 = (next(&mut rng) & 0xFFFF_FFFF) as u32;
    let dacp_id = hex(next(&mut rng), 16);
    let client_instance = hex(next(&mut rng), 16);
    let active_remote = hex(next(&mut rng), 8);
    let ssrc = (next(&mut rng) & 0xFFFF_FFFF) as u32;

    let mut aes_key = [0u8; 16];
    let mut aes_iv = [0u8; 16];
    for byte in aes_key.iter_mut() {
        *byte = (next(&mut rng) & 0xFF) as u8;
    }
    for byte in aes_iv.iter_mut() {
        *byte = (next(&mut rng) & 0xFF) as u8;
    }
    let rsaaeskey = b64(&crypto::wrap_aes_key(&aes_key));
    let aesiv_b64 = b64(&aes_iv);
    let uri = format!("rtsp://{remote_ip}/{session}");

    log(format!("target {host}  local {local}  session {session}"));

    let mut client = RtspClient::connect(&host, TIMEOUT).map_err(|e| format!("connect: {e}"))?;
    client.identity_headers = vec![
        ("Client-Instance".to_string(), client_instance),
        ("DACP-ID".to_string(), dacp_id),
        ("Active-Remote".to_string(), active_remote),
    ];

    report(
        log,
        "OPTIONS",
        &client
            .request("OPTIONS", "*", &[], &[])
            .map_err(|e| e.to_string())?,
    );

    let sdp = build_sdp(session, &local, &remote_ip, &rsaaeskey, &aesiv_b64);
    let mut challenge_bytes = [0u8; 16];
    for byte in challenge_bytes.iter_mut() {
        *byte = (next(&mut rng) & 0xFF) as u8;
    }
    let challenge = b64(&challenge_bytes);

    let announce = client
        .request(
            "ANNOUNCE",
            &uri,
            &[
                ("Content-Type", "application/sdp".to_string()),
                ("Apple-Challenge", challenge),
            ],
            sdp.as_bytes(),
        )
        .map_err(|e| format!("announce: {e}"))?;
    report(log, "ANNOUNCE", &announce);
    if !announce.is_ok() {
        return Err("receiver rejected the announcement".to_string());
    }

    let control_sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let timing_sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let control_port = control_sock.local_addr().map_err(|e| e.to_string())?.port();
    let timing_port = timing_sock.local_addr().map_err(|e| e.to_string())?.port();
    spawn_timing_server(timing_sock, alive.clone());

    // `interleaved=0-1` is meaningless for a UDP stream and is not used by this
    // sender, but it is kept because the tested receiver accepts the transport
    // line with it and there is no reason to risk the handshake.
    let transport = format!(
        "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={control_port};timing_port={timing_port}"
    );
    let setup = client
        .request("SETUP", &uri, &[("Transport", transport)], &[])
        .map_err(|e| format!("setup: {e}"))?;
    report(log, "SETUP", &setup);
    if !setup.is_ok() {
        return Err("receiver rejected the transport".to_string());
    }
    let transport_hdr = setup.header("Transport").unwrap_or("").to_string();
    let session_id = setup.header("Session").unwrap_or("1").to_string();
    let server_port = transport_param(&transport_hdr, "server_port").unwrap_or(6000);
    let control_remote = transport_param(&transport_hdr, "control_port").unwrap_or(6001);

    RTP_NOW.store(60_000, Ordering::Relaxed);
    TIMING_REQUESTS.store(0, Ordering::Relaxed);
    spawn_sync_sender(remote_ip.clone(), control_remote, alive.clone());

    if !cfg.set_volume {
        log("leaving receiver volume untouched (shared with TV)".to_string());
    }
    let vol = format!("volume: {:.1}\r\n", cfg.volume_db);
    if cfg.set_volume {
        match client.request(
            "SET_PARAMETER",
            &uri,
            &[
                ("Session", session_id.clone()),
                ("Content-Type", "text/parameters".to_string()),
            ],
            vol.as_bytes(),
        ) {
            Ok(r) => log(format!(
                "set volume {:.1} dB -> {} {}",
                cfg.volume_db, r.code, r.reason
            )),
            Err(e) => log(format!("set volume failed: {e}")),
        }
    }

    let read_volume = |client: &mut RtspClient, session: &str| -> String {
        match client.request(
            "GET_PARAMETER",
            &uri,
            &[
                ("Session", session.to_string()),
                ("Content-Type", "text/parameters".to_string()),
            ],
            b"volume",
        ) {
            Ok(r) => String::from_utf8_lossy(&r.body).trim().to_string(),
            Err(e) => format!("read failed: {e}"),
        }
    };

    if cfg.set_volume {
        log(format!(
            "volume before: {:?}",
            read_volume(&mut client, &session_id)
        ));
    }

    // The reference sender sends the parameter with no trailing CRLF; a
    // trailing CRLF appears to be silently ignored by this receiver.
    if cfg.set_volume {
        let _ = client.request(
            "SET_PARAMETER",
            &uri,
            &[
                ("Session", session_id.clone()),
                ("Content-Type", "text/parameters".to_string()),
            ],
            format!("volume: {:.1}", cfg.volume_db).as_bytes(),
        );
        log(format!(
            "after set (no CRLF): {:?}",
            read_volume(&mut client, &session_id)
        ));
    }

    if cfg.probe_only {
        let _ = client.request("TEARDOWN", &uri, &[("Session", session_id)], &[]);
        log("probe complete: no audio sent".to_string());
        return Ok(());
    }

    // Long-poll /feedback on its own connection for the whole session.
    {
        let headers = client.identity_headers.clone();
        let host_fb = host.clone();
        let alive_fb = alive.clone();
        std::thread::spawn(move || {
            if let Ok(mut fb) = RtspClient::connect(&host_fb, Duration::from_secs(30)) {
                fb.identity_headers = headers;
                while alive_fb.load(Ordering::Relaxed) {
                    if fb.request("POST", "/feedback", &[], &[]).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        });
        std::thread::sleep(Duration::from_millis(100));
    }

    // The receiver answers RECORD only after audio flows, so never await it.
    client
        .send_only("RECORD", &uri, &[], &[])
        .map_err(|e| format!("record: {e}"))?;

    let destination = format!("{remote_ip}:{server_port}");
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
    sock.connect(&destination)
        .map_err(|e| format!("udp connect: {e}"))?;

    if cfg.source == Source::System {
        stream_system(cfg, log, stop, alive, &sock, ssrc, &aes_key, &aes_iv)?;
        // The deferred RECORD reply is diagnostic only; never wait long for it.
        let _ = client.set_read_timeout(Duration::from_millis(200));
        if let Ok(r) = client.read_pending() {
            log(format!(
                "RECORD reply: {} {}",
                r.code,
                r.header("Audio-Latency").unwrap_or("-")
            ));
        }
        let _ = client.request("TEARDOWN", &uri, &[("Session", session_id)], &[]);
        log("session closed".to_string());
        return Ok(());
    }

    let total_frames = (SAMPLE_RATE as f64 * cfg.seconds) as u32;
    let mut seq: u16 = 0;
    let mut timestamp: u32 = 60_000;
    let mut sent: u32 = 0;
    let mut packets_sent: u32 = 0;
    let start = std::time::Instant::now();
    log(format!("streaming {:.1} s", cfg.seconds));
    let mut alac = crate::alac::Alac::new();

    while sent < total_frames {
        if stop.load(Ordering::Relaxed) {
            log("stopped by user".to_string());
            break;
        }
        let n = FRAMES_PER_PACKET.min(total_frames - sent);
        let packet = audio_packet(
            &mut alac,
            seq,
            timestamp,
            ssrc,
            n,
            cfg.tone_hz,
            sent,
            cfg.amplitude,
            &aes_key,
            &aes_iv,
        );
        sock.send(&packet).map_err(|e| format!("send: {e}"))?;
        RTP_NOW.store(timestamp, Ordering::Relaxed);

        seq = seq.wrapping_add(1);
        timestamp = timestamp.wrapping_add(n);
        sent += n;
        packets_sent += 1;
        if packets_sent % 250 == 0 {
            log(format!(
                "  {} packets, {} timing requests",
                packets_sent,
                TIMING_REQUESTS.load(Ordering::Relaxed)
            ));
        }
        pace(start, sent);
    }

    log(format!(
        "sent {} packets, {} timing requests from receiver",
        seq as u32,
        TIMING_REQUESTS.load(Ordering::Relaxed)
    ));

    let _ = client.set_read_timeout(Duration::from_millis(200));

    if let Ok(r) = client.read_pending() {
        log(format!(
            "RECORD reply: {} {}",
            r.code,
            r.header("Audio-Latency").unwrap_or("-")
        ));
    }
    let _ = client.request("TEARDOWN", &uri, &[("Session", session_id)], &[]);
    log("session closed".to_string());
    Ok(())
}
