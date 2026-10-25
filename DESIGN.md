# raop-sender — design notes

Goal: a Rust binary that makes an AirPlay 1 receiver usable as an audio sink from
Windows, with no third-party sender.

## Target facts (probed live against the test receiver)

| Item | Value |
|---|---|
| mDNS name | `Freebox Player` (host `Freebox-Player`) |
| RAOP service | `_raop._tcp`, **TCP 5000** |
| AirPlay service | `_airplay._tcp`, TCP 7000 |
| RTP media | **server 6000** |
| Control / timing | 6001 / 6002 |
| `am` / model | `AppleTV3,2` (masquerade) |
| `features` | `0x12BFFEBB` — bit 9 Audio, 11 AudioMetaData, 18 AudioReceiver, 23 AudioAES, 28 AudioNTP |
| AirPlay 2 | **absent** — bit 38 / HomeKit pairing not set |
| `et` | `0,1,3,5` → resolves to `encryption=23` (Unencrypted\|RSA\|FairPlay\|FairPlaySAPv25); RSA/AES is used |
| `ek` | `1` — payload encryption required |
| `pw` | `false` — no password, no digest auth |
| `cn` | `0,1,2,3` — PCM, ALAC, and two more |
| `sr/ss/ch` | 44100 / 16 / 2 |
| `pk` | RSA public key for the AES key exchange |
| `tp` | `TCP,UDP` |

## Wire protocol (AirPlay 1 / RAOP) as implemented

1. `OPTIONS * RTSP/1.0` — capability probe.
2. `ANNOUNCE` with SDP. Payload type 96 carries **ALAC**:

   ```
   a=rtpmap:96 AppleLossless
   a=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100
   a=rsaaeskey:<b64>     RSA-OAEP(SHA-1) of a random 16-byte AES key
   a=aesiv:<b64>         random 16-byte IV
   ```

3. `SETUP` with `Transport: RTP/AVP/UDP;unicast;mode=record` → device replies with
   `server_port` (6000), `control_port` (6001), `timing_port` (6002).
4. `RECORD` — sent **write-only**, never awaited (see traps).
5. Stream over UDP: 352 frames per packet. Per packet:
   - encode the PCM frame as ALAC,
   - AES-128-CBC encrypt only the first `len & ~0xF` bytes; the trailing bytes
     stay in plaintext,
   - prefix the 12-byte RTP header (`0x80 0x60`, seq, timestamp, ssrc), send to
     `server_port`.
6. Sync packets to the control port every 250 ms, and a small timing server that
   answers the receiver's 0xD2 requests.
7. `TEARDOWN`.

## Why ALAC and not PCM

`cn=0` advertises PCM, and `a=rtpmap:96 L16/44100/2` is accepted with `200 OK`.
It still produces **loud static**: payload type 96 is decoded as AppleLossless
regardless of the announced rtpmap, so raw PCM is reinterpreted as a compressed
bitstream. ALAC is not an optimisation here; it is the only thing that works.

## The AES framing trap

The commonly-implemented "first two bytes are the length" scheme is wrong for
this class of receiver and produces a click on every packet, because it corrupts
the first AES block:

```c
int aeslen = len & ~0xf;              // only this prefix is encrypted
aes_decrypt(data, aeslen, ...);
memcpy(out + aeslen, data + aeslen, len - aeslen);   // tail is plaintext
```

There is no length prefix. Anything that overwrites ciphertext bytes with a
length will be audible as a buzz at the packet rate.

## Transport

UDP to `server_port`. TCP interleaved is accepted at `SETUP` but the payload
resets the connection on this receiver.

## Volume

`SET_PARAMETER` with `volume: <dB>` works, but the value is the receiver's
**master** level, shared with its TV/HDMI output. A background sender must leave
it alone, which is why volume writes are opt-in (`Settings::set_volume`) and the
daemon runs with them disabled.

DACP (`SETPROPERTY`, `DMCP.DEVICE-VOLUME`) is an alternative channel for the same
level and is not implemented.

## Status

- [x] Rust RTSP session + RSA/AES handshake
- [x] ALAC RTP streaming
- [x] WASAPI capture of a virtual cable, with real mix-format conversion
- [x] Windowless daemon, logon task, firewall rule
- [ ] Retransmission of lost packets (receiver requests on the control port are
      currently ignored)
- [ ] Measured end-to-end latency (acoustic cross-correlation)

## Sequence the device answers cleanly

```
OPTIONS *                  200
GET /info                  200  (plist; omit Content-Type, or it may not answer)
ANNOUNCE + Apple-Challenge 200  -> device returns a 512-byte Apple-Response
SETUP  (UDP)               200  -> server_port=6000 control_port=6001 timing_port=6002
SET_PARAMETER volume       200  (optional)
FLUSH  (RTP-Info)          200  -> device echoes rtptime
<stream RTP>               accepted, no reset
TEARDOWN                   200
```

`RECORD` is the trap: this receiver never answers it before audio flows. Send it
write-only and stream; the reply arrives afterwards as `200 OK` with
`Audio-Latency: 2205`. Waiting for it deadlocks the session.

Other confirmed details:

- `Apple-Challenge` (16 random bytes, base64) is validated: the device replies
  with a real `Apple-Response`. Verification on the sender side is optional.
- `c=` in the SDP names the **receiver**, `o=` names the sender.
- Volume is written as `volume: <dB>` with **no** trailing CRLF.

## Working recipe (confirmed by device behaviour)

Four things had to be true at once. Any one missing produced silence with all
200s:

1. **Inbound UDP allowed.** Windows Firewall blocks inbound by default on every
   profile. Without a rule the receiver's timing requests never arrive and it
   silently discards every audio datagram.
   `New-NetFirewallRule -DisplayName 'raop-sender' -Direction Inbound -Program
   '<exe>' -Action Allow -Protocol UDP`
2. **RECORD must not be awaited.** Deferred reply: `200 OK` + `Audio-Latency: 2205`.
3. **Sync packets on the control port** every ~250 ms (`0x80/0x90 0xD4 0x0007`,
   rtp, ntp_sec, ntp_frac, rtp) — the playback anchor.
4. **AES-128-CBC payload encryption** with the RSA-OAEP wrapped key in the SDP.

Observable proof of real playback, without listening:

```
timing request: 32 bytes from <receiver>:6002    (repeated)
RECORD (deferred)      200 OK  Audio-Latency: 2205
```

The timing requests are the signal: the receiver only asks for the clock once it
has committed to decoding.

## The standby mute (connects but silent)

Measured with the TV off:

```
set volume -20.0 dB -> 200 OK
volume before:        "volume: -144.000000"
after set (no CRLF):  "volume: -144.000000"
after set 0.0:        "volume: -144.000000"
```

`-144` is the AirPlay mute floor. In standby the receiver completes
OPTIONS/ANNOUNCE/SETUP, accepts every RTP packet, returns 200 to every
`SET_PARAMETER volume`, and outputs nothing. `200 OK` is not an acknowledgement
of effect.

With the unit awake the same probe returns the values it was given, so a volume
write is honoured only once it is out of standby. Diagnostic: read the volume back
before streaming. `-144` means muted, and no level on our side can help.

Consequence: **AirPlay cannot serve this receiver while it is asleep.** Audio in
that state has to use a different input — Bluetooth, or S/PDIF — not RAOP.

## Volume range (measured)

The receiver does not fade to silence — it **mutes** below roughly **-28 dB**
(measured between -27 and -29). Usable range is about **-26 … 0 dB**, so the GUI
slider is clamped to that and the floor is labelled rather than left as a silent
trap. Anything below it produces exactly nothing, with every protocol step still
returning 200.

## Latency

Sender-side delay is entirely ours and is logged:

```
 500 packets, 0 underruns, pipeline delay 60 ms
```

| Component | Default | Knob |
|---|---|---|
| Audio buffered before the first packet | 60 ms | `RAOP_PRIME_MS` |
| Backlog retained when the stream starts | 60 ms | `RAOP_BUFFER_MS` |
| Receiver's advertised latency | ~50 ms | not controllable here |
| LAN transit | < 5 ms | — |

The silence gate also prevents a stale backlog: while waiting for non-silent
audio the queue would otherwise grow, and playing it from the front made playback
seconds late. The gate discards everything but the last `RAOP_BUFFER_MS`.

## Session lifecycle

The stream starts only when captured audio becomes non-silent. After that, silence
does **not** idle the session: a silent packet every 8 ms keeps the RTP timeline
alive, so pausing playback and resuming costs nothing.

`RAOP_IDLE_MS` (default 10 minutes) exists only to re-arm a receiver that fell
asleep mid-session. It was 2 minutes and that was wrong — it tore down sessions
that were perfectly healthy, so every long pause was followed by a reconnect the
listener could hear.

One case remains undetectable from the sender: audio keeps arriving but the
receiver has gone to standby, so it looks identical to a healthy stream. The
daemon keeps a one-hour safety net for that. Polling `GET_PARAMETER volume` and
watching for the `-144` mute floor would distinguish the two, and is the obvious
next improvement.

## Capture notes

The cable reports **32-bit float**, and a render endpoint's loopback yields
nothing unless that endpoint is actively rendering. Two independent failure modes
worth remembering:

- Assuming 16-bit PCM when the device emits float → loud static.
- Capturing a render device that nothing is playing to → zero bytes forever,
  which looks identical to a broken protocol.

## Diagnostic modes

```
raop-sender --probe-volume --host IP:PORT [--volume-db X]   # handshake only, silent
raop-sender --play --host IP:PORT [--seconds N] [--volume-db X]
```

`--probe-volume` never sends RECORD or RTP, so it produces no sound.
