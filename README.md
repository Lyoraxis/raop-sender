# raop-sender

[![ci](https://github.com/Lyoraxis/raop-sender/actions/workflows/ci.yml/badge.svg)](https://github.com/Lyoraxis/raop-sender/actions/workflows/ci.yml)
![platform](https://img.shields.io/badge/platform-windows%20x64-blue)
![licence](https://img.shields.io/badge/licence-MIT-green)

**AirPlay 1 / RAOP sender for Windows, in Rust.** Turn an AirPlay 1 speaker into a
normal Windows output device — so any app can play to it, with no AirPlay client,
no proprietary software, and no GUI.

Tested against a **Freebox Delta Player (Devialet)**. Should work with any
receiver that advertises AirPlay 1.

> **Using an AI agent?** Skip to [Agent quickstart](#agent-quickstart) — there is a
> copy/paste prompt that carries every hard-won constraint in this repo.

**Keywords:** AirPlay on Windows, RAOP sender, AirPlay 1 sender, Windows audio
output to AirPlay, virtual audio device, ALAC, WASAPI loopback, Freebox, Devialet,
Rust, AirPlay without iTunes, AirPlay receiver as Windows speaker.

---

## Agent quickstart

Paste this into any coding agent working in this repository:

```text
raop-sender is a Rust AirPlay 1 / RAOP sender for Windows. It captures audio from
a virtual cable (VB-CABLE) and streams it to an AirPlay 1 receiver as ALAC.

Goal: keep a windowless background sender that behaves like a normal Windows
output device. Audio goes: app -> CABLE Input -> CABLE Output -> ALAC -> RAOP.

Before changing anything, run `raop-sender --doctor`: it reports the receiver, the
capture format, the level the cable is carrying, the firewall rule and the
scheduled task in one shot, and it plays nothing.

Non-negotiable protocol constraints (each one cost hours to find; do not "clean
them up"):

1. Payload type 96 MUST carry ALAC. Sending raw PCM makes the receiver emit
   loud static. Announce `a=rtpmap:96 AppleLossless` with the ALAC magic cookie as
   `a=fmtp`: 352 0 16 40 10 14 2 255 0 0 44100.
2. AES-128-CBC encrypts only the first `len & ~0xF` bytes of the payload. There is
   NO length prefix. Leave the trailing bytes in plaintext.
3. Never await the RECORD reply; the receiver answers only after audio flows.
   Send it write-only, then stream.
4. Send sync packets (0x90/0x80, 0xD4, 0x0007) to the receiver's control port
   every ~250 ms, and answer its 0xD2 timing requests on the sender's timing port.
5. An inbound UDP firewall rule for the sender executable is MANDATORY. Without
   it every RTSP request still returns 200 OK and no audio is produced.
6. Do NOT send SET_PARAMETER volume unless the user explicitly asks. That is the
   receiver's master volume, shared with its TV/HDMI output, so writing it turns
   the television up or down. `Settings::set_volume = false` in the daemon.
7. Stream nothing until captured audio is non-silent. An RTP stream of digital
   zeros makes receivers click. The gate is `session::AUDIO_GATE`, deliberately
   low (16 of 32768): a quiet source or a low Windows volume leaves peaks in the
   low hundreds, and a gate set near the source level silently turns "a bit quiet"
   into "no sound at all".
8. Read the endpoint's real mix format and convert it in `src/pcm.rs`. Assuming
   16-bit PCM when the device emits 32-bit float yields loud static. That module is
   platform-neutral and unit-tested for exactly this reason — keep it that way.
9. Send `RECORD` only once captured audio is non-silent. A receiver drops a
   recording session that receives no packets, so an early `RECORD` means the first
   sound plays into a session that no longer exists.

Latency lives in `RAOP_PRIME_MS` and `RAOP_BUFFER_MS` (default 60 ms each) plus
`src/capture.rs`. Do not reintroduce large buffers.

Configuration comes from the environment, with raop-sender.env next to the
executable as a fallback: RAOP_HOST (required), RAOP_DEVICE (default CABLE),
RAOP_PRIME_MS, RAOP_BUFFER_MS. Never hardcode a receiver address.

Before changing protocol code, read DESIGN.md. Verify builds with
`cargo build --release` on the Windows MSVC toolchain; this is Windows-only code
(WASAPI) and will not link on Linux/macOS.
```

---

## How it works

```
Windows app ──► "CABLE Input" (virtual endpoint, VB-CABLE)
                        │
                        ▼
              "CABLE Output" (WASAPI capture)
                        │  float32 → i16 / 44.1 kHz / stereo
                        ▼
                  ALAC encoder (352 frames/packet)
                        │  AES-128-CBC, RSA-OAEP key in SDP
                        ▼
                  RAOP over UDP ──► receiver (port 5000 / 6000 / 6001 / 6002)
```

Protocol sequence: `OPTIONS` → `ANNOUNCE` → `SETUP` → `SET_PARAMETER` (optional)
→ `RECORD` (not awaited) → RTP audio → `TEARDOWN`, with sync packets to the
receiver's control port and a timing server on the sender side.

## Compatibility

| Receiver | Status |
|---|---|
| Freebox Delta Player (Devialet), `AppleTV3,2`, features `0x12BFFEBB` | **works** — the device this was built and tested against |
| Other AirPlay 1 receivers | untested; the protocol path is standard, but nothing else has been confirmed |

Two properties decide whether a receiver can work at all:

- it must advertise **AirPlay 1** (no AirPlay 2 / HomeKit pairing bit), and
- it must accept **RSA/AES** payload encryption (`ek=1`).

If you test another device, the log tail from `daemon.log` is the useful thing to
attach to an issue.

## Requirements

| Requirement | Notes |
|---|---|
| Windows 10/11 (x64) | capture uses WASAPI, so the sender is Windows-only |
| Rust **1.85+** with the MSVC toolchain | `x86_64-pc-windows-msvc` (the WASAPI binding requires 1.85) |
| Visual Studio Build Tools (C++ workload) | provides the linker; run `vcvars64.bat` in the build shell |
| **VB-CABLE** virtual audio device | https://vb-audio.com/Cable/ — free, signed driver |
| A RAOP/AirPlay-1 receiver | must advertise AirPlay 1. AirPlay-2-only devices will not work |

`VB-CABLE` is only used to create the endpoint. Any virtual audio cable that
exposes a WASAPI capture endpoint will do — point `RAOP_DEVICE` at it.

## Build

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
cargo build --release
```

Binaries land in `target\release\`:

| Binary | Purpose |
|---|---|
| `raop-daemon.exe` | windowless background sender (the product) |
| `raop-sender.exe` | CLI, useful for diagnosis |
| `raop-gui.exe` | small test GUI with a volume slider and Play/Stop |

## Configuration

All configuration is environment variables — nothing site-specific is compiled in.

| Variable | Default | Meaning |
|---|---|---|
| `RAOP_HOST` | *(required)* | `ip:port` of the receiver's RAOP service, normally `:5000` |
| `RAOP_DEVICE` | `CABLE` | case-insensitive substring of the capture device's friendly name |
| `RAOP_PRIME_MS` | `60` | audio buffered before the first packet is sent |
| `RAOP_BUFFER_MS` | `60` | backlog retained when the stream starts; pure added latency |
| `RAOP_IDLE_MS` | `600000` | silence tolerated before the session is recycled; short pauses cost nothing |

`RAOP_HOST` has no default on purpose — nothing site-specific is compiled in.
The CLI takes `--host` as an alternative, and the GUI has an editable field.

The daemon also reads a `raop-sender.env` file next to the executable, using the
same `KEY=VALUE` syntax, for any variable not already in the environment. That
is what `install.ps1` writes, so a scheduled task does not have to wait for a
fresh logon to see the configuration.

`RAOP_PRIME_MS` + `RAOP_BUFFER_MS` are the entire latency budget you control.
Raising them trades delay for resistance to dropouts.

## Install

1. Install VB-CABLE and reboot.
2. Rename the new playback endpoint to taste (Settings → System → Sound → All
   sound devices → *CABLE Input* → Rename).
3. Build, then run `install.ps1` **as administrator** from `target\release\`:

```powershell
.\install.ps1 -Receiver "192.0.2.10:5000"
```

It adds the inbound UDP firewall rule the receiver needs in order to send timing
requests back, writes `raop-sender.env` next to the daemon, and registers a logon
scheduled task.

### The firewall rule is mandatory

A RAOP receiver sends timing requests **back** to the sender. Windows Firewall
drops unsolicited inbound UDP by default, and the failure mode is silent: every
RTSP request returns `200 OK` and no audio is produced. If there is no audio,
check the firewall rule first.

## Troubleshooting

Start with `raop-sender --doctor`; it names the failing precondition directly.

| Symptom | Cause |
|---|---|
| Silence, all `200 OK` | only one of the four conditions is met: inbound UDP firewall rule, `RECORD` not awaited, sync packets every ~250 ms, AES key wrapped in SDP |
| Silence, `volume: -144.000000` | receiver is asleep/standby and silently ignores volume writes; wake it |
| Silence below ~-28 dB | the tested Player **mutes** rather than fading; keep volume above it |
| Noise instead of audio | payload type 96 is decoded as **ALAC**; sending raw PCM produces static |
| Clicks ~125/s | AES framing: only `len & ~0xF` bytes are encrypted and there is **no** length prefix |
| No audio at all, `0 packets` | WASAPI loopback only delivers bytes while the device renders; capture a real capture device (the cable), not a render device |

## Latency

Sender-side delay is fully accounted for and logged:

```
 500 packets, 0 underruns, pipeline delay 60 ms
```

End-to-end latency is the sum of the retained backlog, the receiver's own jitter
buffer, its advertised `Audio-Latency` (2205 frames ≈ 50 ms on the tested
device), and LAN transit. The receiver's contribution is not controllable from
here.

### Command line

```bash
raop-sender --doctor --host 192.0.2.10:5000          # diagnose, plays nothing
raop-sender --play --host 192.0.2.10:5000 --seconds 3 --volume-db -24
raop-sender --probe-volume --host 192.0.2.10:5000   # silent: handshake and volume only
```

`--host` is required unless `RAOP_HOST` is set. `--play` is required to emit
audio; `--doctor` and `--probe-volume` never send RTP.

### `--doctor`

Every failure this project has seen in the field was silent: `200 OK` on every
request and no sound. `--doctor` walks the preconditions in one run, reads the
cable locally and opens no session:

```
  [ok  ] config         RAOP_HOST=192.0.2.10:5000, RAOP_DEVICE=CABLE, raop-sender.env present
  [ok  ] receiver tcp   192.0.2.10:5000 accepts connections
  [ok  ] receiver /info 200 OK (AppleTV3,2)
  [ok  ] capture device CABLE Output (VB-Audio Virtual Cable) at 44100 Hz, 32-bit, 2 ch, float=true, 44541 frames heard
  [ok  ] audio level    peak 114 (-49.2 dBFS), above the 16 start gate
  [ok  ] firewall       rule 'AirPlay Sender' is present
  [ok  ] scheduled task 'AirPlay Sender' is registered
```

Exit code is non-zero when any check fails. The audio-level check is the one that
distinguishes "the sender is broken" from "nothing is being sent to it": it reports
the peak it can hear and compares it with the gate the session waits for. It is
skipped when the daemon is running, because VB-CABLE feeds only its first capture
client — a second reader measures noise, and a false "nothing is routed" is worse
than no answer. In that case read the `peak` field the daemon logs with every stats
line.

## Tests

```bash
cargo test
```

26 unit tests cover the parts that are easy to get subtly wrong and hard to notice
by ear:

- **AES framing** — length preserved, only the aligned prefix encrypted, tail left
  as plaintext, round-trips.
- **ALAC packet encoding** — non-empty, smaller than raw PCM, deterministic for a
  fresh encoder, stable over sequential packets.
- **Format conversion** (`src/pcm.rs`) — float32/float64/int16/int24/int32 decoding,
  mono duplication, ignored extra channels, clipping, 48 kHz resampling over one
  second and across 100 chunks, chunk-boundary independence, and nonsense geometry.
  This module is platform-neutral so it can be tested off Windows.

The streaming path itself is Windows-only and has no automated test — it needs a
real endpoint and a real receiver. `--doctor` covers its preconditions.

## Deployment hazards

These are not bugs to fix; they are properties of the stack. Know them before
calling the sender broken.

| Hazard | What happens | What to do |
|---|---|---|
| **The capture endpoint is exclusive** | VB-CABLE feeds its first capture client. A second one — another sender, OBS, Sonar's router — reads noise, so the daemon goes deaf **silently**. | Run one reader only. If audio stops without a firewall or routing change, check what else opened `CABLE Output`. |
| **`raop-gui.exe` competes with the daemon** | Same endpoint. Start it *instead of* the daemon, never alongside, and stop whichever one you are not using. | Use the GUI to test a receiver, the daemon to actually listen. |
| **The receiver's volume is shared with its TV** | `SET_PARAMETER volume` writes the master level of the device, which also controls its HDMI output. | The daemon never writes it. Set volume on the receiver or its remote. |
| **Standby reads `-144.000000` and ignores writes** | The device reports a sentinel volume and silently drops `SET_PARAMETER` with `200 OK`. | Wake it (TV on, or its remote) before streaming. No sender-side fix exists. |
| **Below roughly -28 dB the receiver mutes** | It does not fade, it cuts out. | Keep the receiver's volume above that. |
| **Sonar and similar mixers sit in the path** | Apps are routed by the mixer, so "music is playing" can mean "playing to a different device". | Check the mixer's output device, not just the Windows default. |

## Limitations

- **Windows only.** Capture is WASAPI; the crate is `cfg(windows)`.
- **AirPlay 1 only.** No AirPlay 2 pairing, so AirPlay-2-only devices will not work.
- **No retransmission.** The receiver may request missing packets on its control
  port; those requests are ignored, so a lossy link causes dropouts.
- **A receiver in standby is unreachable.** It accepts the stream and outputs
  nothing (see the troubleshooting table).
- **Latency is measured on the sender side only.** The receiver's own buffering
  is added on top and is not controllable from here.
- **A receiver that falls asleep mid-playback is only recovered on the next
  session rotation.** Silence is detected and recycled after `RAOP_IDLE_MS`, but
  a receiver that sleeps while audio is still flowing looks identical to a working
  one from here. A periodic volume read would distinguish them; not implemented.

## Layout

| File | Contents |
|---|---|
| `src/session.rs` | RAOP handshake, RTP pacing, sync packets, timing server |
| `src/doctor.rs` | `--doctor` checks, one per silent failure mode |
| `src/crypto.rs` | RSA-OAEP key wrap, AES-128-CBC payload encryption |
| `src/alac.rs` | ALAC encoding wrapper (352 frames/packet) |
| `src/capture.rs` | WASAPI capture from the chosen endpoint (Windows-only) |
| `src/pcm.rs` | endpoint bytes → 44.1 kHz/16-bit/stereo; platform-neutral and tested |
| `src/rtsp.rs` | dependency-free RTSP client |
| `DESIGN.md` | wire-level notes and the failure modes found while building it |
| `CHANGELOG.md` | what changed, and why each change mattered |

## Notes on the RSA key

`src/crypto.rs` embeds the 2048-bit RAOP public key that AirPlay 1 receivers
expect. It is a **public** key, taken from the open-source `shairport-sync`
project, and is not a secret. It is the same key every third-party AirPlay 1
sender uses.

## License

MIT — see `LICENSE`.
