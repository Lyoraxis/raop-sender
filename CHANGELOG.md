# Changelog

All notable changes to this project. Dates are ISO-8601. This project follows
[Semantic Versioning](https://semver.org/).

## [0.2.0] — 2026-10-25

First version that can be handed to someone else without a walkthrough. Everything
below was found by running it against a real receiver, and the theme is the same
each time: **the failure mode is silence, never an error.**

### Fixed

- **`RECORD` is sent only once there is audio to send.** It used to be issued at
  startup, so the receiver held a recording session that received no packets and
  could drop it — the first sound then played into a session that no longer
  existed. Sessions now start when audio appears, not when the process does.
- **The start gate no longer requires loud audio.** It sat at 200 of 32768
  (-44 dBFS), which is not "non-silent" but "loud". Music was measured starting at
  peak 68 and running at 119-121, i.e. entirely below the old gate, so quiet
  playback never started. It is 16 (-66 dBFS) now, which still separates digital
  zeros from signal — the only thing the gate was ever asked to do.
- **Late frames are waited for instead of zero-filled.** Pacing runs at exactly
  real time, so ordinary scheduling jitter could leave a packet a millisecond
  short; it was padded with zeros, which is an audible 8 ms gap. A short packet now
  waits up to 30 ms for its frames, and the pacer catches up.

### Added

- **`--doctor`.** Checks the configuration, receiver reachability, the receiver's
  `/info` document, the capture device and its real mix format, the level the cable
  is actually carrying, the firewall rule and the scheduled task; exits non-zero if
  anything failed. Plays nothing.
- **The transmitted peak is logged** with every stats line, so a log can answer
  "is this silence or music?" — something the receiver's replies never reveal.
- **`CHANGELOG.md`**, and a *Deployment hazards* section in the README covering the
  exclusive capture endpoint, the GUI competing with the daemon, the receiver's
  shared volume, the standby sentinel and mixer routing.

### Changed

- The endpoint-bytes → 44.1 kHz/16-bit/stereo conversion moved out of the
  Windows-only capture module into `src/pcm.rs`: platform-neutral, 13 tests,
  covering float32/float64/int16/int24/int32, mono duplication, clipping, 48 kHz
  resampling and chunk-boundary independence. It also stopped allocating per frame.
- Configuration loading moved into `raop_sender::config` so the daemon, the CLI and
  the doctor cannot disagree about `raop-sender.env`.
- 26 unit tests, up from 5.

## [0.1.0] — 2026-10-25

First working version: `ANNOUNCE`/`SETUP`/`RECORD`, ALAC over RTP, AES-128-CBC with
the RAOP framing, sync and timing packets, WASAPI capture of a virtual cable, a
windowless daemon and an installer that registers a logon task and the inbound UDP
rule the receiver needs in order to send timing requests back.

[0.2.0]: https://github.com/Lyoraxis/raop-sender/releases/tag/v0.2.0
[0.1.0]: https://github.com/Lyoraxis/raop-sender/releases/tag/v0.1.0
