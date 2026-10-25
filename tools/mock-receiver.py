#!/usr/bin/env python3
"""Minimal RAOP/AirPlay-1 receiver, for testing the sender without a real device.

Answers the RTSP handshake the sender performs and counts the RTP packets that
arrive, so end-to-end behaviour can be checked on a machine with no AirPlay
hardware and without making a sound. Nothing leaves the loopback interface.

Usage:
    python3 tools/mock-receiver.py --port 5000 --seconds 15
    raop-sender --play --host 127.0.0.1:5000

Prints one summary line per second: packets received, and the RTP payload size,
which must be constant for ALAC framing to be intact.
"""

import argparse
import socket
import struct
import sys
import threading
import time

RTSP_PORT = None
UDP_SERVER = None
UDP_CONTROL = None
UDP_TIMING = None

lock = threading.Lock()
stats = {
    "packets": 0,
    "bytes": 0,
    "sync": 0,
    "timing_requests": 0,
    "min_size": None,
    "max_size": None,
    "seq_gaps": 0,
    "ts_bad": 0,
    "first_ts": None,
    "last_seq": None,
    "last_ts": None,
    "first_time": None,
    "last_time": None,
}
stop = threading.Event()


def log(line):
    print(line, flush=True)


def handle_rtsp(conn, addr):
    """One RTSP connection. Replies 200 to everything, which is what matters."""
    with conn:
        buf = b""
        while not stop.is_set():
            try:
                data = conn.recv(65536)
            except OSError:
                return
            if not data:
                return
            buf += data
            # A request is a header block plus Content-Length bytes of body. The
            # body must be consumed: ANNOUNCE carries SDP, and leaving it in the
            # buffer makes the next request parse out of the middle of the SDP.
            while True:
                if b"\r\n\r\n" not in buf:
                    break
                head, rest = buf.split(b"\r\n\r\n", 1)
                lines = head.split(b"\r\n")
                method, uri, _ = (lines[0].decode("latin1").split(" ") + ["", "", ""])[:3]
                headers = {}
                for line in lines[1:]:
                    if b":" in line:
                        k, v = line.split(b":", 1)
                        headers[k.strip().lower().decode("latin1")] = v.strip().decode("latin1")

                length = int(headers.get("content-length", "0") or 0)
                if len(rest) < length:
                    break  # wait for the rest of the body
                body_in = rest[:length]
                buf = rest[length:]

                cseq = headers.get("cseq", "0")
                extra = ""
                body = b""
                if method == "OPTIONS":
                    extra = (
                        "Public: OPTIONS, ANNOUNCE, SETUP, RECORD, "
                        "SET_PARAMETER, GET_PARAMETER, FLUSH, TEARDOWN, POST, GET\r\n"
                    )
                elif method == "ANNOUNCE":
                    sdp = body_in.decode("latin1", "replace")
                    for needed in ("rsaaeskey", "aesiv", "AppleLossless", "fmtp:96"):
                        if needed not in sdp:
                            log(f"  ! ANNOUNCE SDP is missing {needed}")
                    if "AppleLossless" not in sdp:
                        log("  ! payload type 96 is not ALAC: the receiver would emit static")
                    extra = "Apple-Response: " + "A" * 64 + "\r\n"
                elif method == "SETUP":
                    extra = (
                        "Session: 1\r\n"
                        f"Transport: RTP/AVP/UDP;unicast;mode=record;"
                        f"server_port={UDP_SERVER.getsockname()[1]};"
                        f"control_port={UDP_CONTROL.getsockname()[1]};"
                        f"timing_port={UDP_TIMING.getsockname()[1]}\r\n"
                        "Audio-Jack-Status: connected\r\n"
                    )
                elif method == "GET" and uri.startswith("/info"):
                    plist = (
                        '<?xml version="1.0" encoding="UTF-8"?>\n'
                        "<plist><dict>\n"
                        "<key>model</key>\n<string>MockReceiver1,1</string>\n"
                        "<key>features</key>\n<string>0x5A7FFFF7,0x1E</string>\n"
                        "</dict></plist>\n"
                    )
                    body = plist.encode()
                    extra = (
                        "Content-Type: text/x-apple-plist+xml\r\n"
                        f"Content-Length: {len(body)}\r\n"
                    )
                elif method == "TEARDOWN":
                    log(f"TEARDOWN   from {addr[0]}")

                reply = (
                    f"RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n{extra}"
                    f"Server: mock-receiver\r\n\r\n"
                ).encode() + body
                conn.sendall(reply)
                if method != "POST":
                    log(f"{method:<10} {uri}")


def udp_loop(name, sock, on_datagram):
    while not stop.is_set():
        try:
            sock.settimeout(0.2)
            data, addr = sock.recvfrom(65536)
        except socket.timeout:
            continue
        except OSError:
            return
        on_datagram(data, addr)


def on_server(data, addr):
    """Count RTP and check the two invariants the wire format depends on:
    the sequence number advances by one, and the timestamp by 352 frames."""
    if len(data) < 12:
        return
    seq, ts = struct.unpack("!HI", data[2:8])
    now = time.time()
    with lock:
        stats["packets"] += 1
        stats["bytes"] += len(data)
        size = len(data) - 12
        stats["min_size"] = size if stats["min_size"] is None else min(stats["min_size"], size)
        stats["max_size"] = size if stats["max_size"] is None else max(stats["max_size"], size)
        if stats["last_seq"] is not None and (stats["last_seq"] + 1) & 0xFFFF != seq:
            stats["seq_gaps"] += 1
        if stats["last_ts"] is not None and (stats["last_ts"] + 352) & 0xFFFFFFFF != ts:
            stats["ts_bad"] += 1
        if stats["first_ts"] is None:
            stats["first_ts"] = ts
            stats["first_time"] = now
        stats["last_seq"] = seq
        stats["last_ts"] = ts
        stats["last_time"] = now


def on_control(data, addr):
    with lock:
        stats["sync"] += 1


def on_timing(data, addr):
    """The sender answers the receiver's timing requests; count both sides."""
    with lock:
        stats["timing_requests"] += 1
    if len(data) >= 32 and data[1] == 0xD2:
        # Turn the request into its reply: byte 1 becomes 0xD3 and the three
        # origin timestamps are echoed back unchanged.
        reply = bytearray(data)
        reply[1] = 0xD3
        try:
            UDP_TIMING.sendto(bytes(reply), addr)
        except OSError:
            pass


def main():
    global RTSP_PORT, UDP_SERVER, UDP_CONTROL, UDP_TIMING

    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=5000, help="RTSP port to listen on")
    ap.add_argument("--seconds", type=float, default=15.0, help="how long to run")
    ap.add_argument("--quiet", action="store_true", help="summary lines only")
    args = ap.parse_args()

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", args.port))
    listener.listen(4)
    listener.settimeout(0.2)
    RTSP_PORT = args.port

    UDP_SERVER = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    UDP_SERVER.bind(("127.0.0.1", 0))
    UDP_CONTROL = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    UDP_CONTROL.bind(("127.0.0.1", 0))
    UDP_TIMING = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    UDP_TIMING.bind(("127.0.0.1", 0))

    log(
        f"mock receiver on 127.0.0.1:{args.port} "
        f"(rtp {UDP_SERVER.getsockname()[1]}, control {UDP_CONTROL.getsockname()[1]}, "
        f"timing {UDP_TIMING.getsockname()[1]}) for {args.seconds:g}s"
    )

    for name, sock, handler in (
        ("server", UDP_SERVER, on_server),
        ("control", UDP_CONTROL, on_control),
        ("timing", UDP_TIMING, on_timing),
    ):
        threading.Thread(target=udp_loop, args=(name, sock, handler), daemon=True).start()

    deadline = time.time() + args.seconds
    last = 0
    while time.time() < deadline:
        try:
            conn, addr = listener.accept()
        except socket.timeout:
            pass
        else:
            threading.Thread(target=handle_rtsp, args=(conn, addr), daemon=True).start()

        now = time.time()
        if now - last >= 1.0:
            last = now
            with lock:
                p = stats["packets"]
                s = stats["sync"]
                lo, hi = stats["min_size"], stats["max_size"]
            if not args.quiet or p:
                log(f"  rtp {p:6d} pkts  payload {lo or 0}-{hi or 0} B  sync {s:5d}")

    stop.set()
    with lock:
        d = dict(stats)
    log("")
    span = (d["last_time"] - d["first_time"]) if d["first_time"] and d["last_time"] else 0.0
    rate = d["packets"] / span if span > 0 else 0.0
    log(
        f"summary: {d['packets']} RTP packets in {span:.2f}s ({rate:.1f}/s), "
        f"{d['bytes']} B, payload {d['min_size']}-{d['max_size']} B, "
        f"{d['sync']} sync, {d['timing_requests']} timing"
    )
    log(
        f"integrity: {d['seq_gaps']} sequence gaps, {d['ts_bad']} timestamp steps "
        "not equal to 352"
    )

    ok = True
    if d["packets"] == 0:
        log("FAIL no RTP: the sender never started streaming")
        ok = False
    if d["seq_gaps"]:
        log("FAIL the RTP sequence number skipped: packets are being lost after send")
        ok = False
    if d["ts_bad"]:
        log("FAIL the timestamp step is not 352 frames: framing is wrong")
        ok = False
    # 352 frames at 44.1 kHz is 7.98 ms, so one second of audio is ~125 packets.
    if d["packets"] and not (120.0 <= rate <= 130.0):
        log(f"FAIL {rate:.1f} packets/s is not the expected ~125: pacing is wrong")
        ok = False
    if d["min_size"] is not None and d["min_size"] == 0:
        log("FAIL an empty payload was sent")
        ok = False
    if d["max_size"] is not None and d["max_size"] > 352 * 4:
        log("FAIL a payload is larger than the raw PCM it encodes: ALAC is not compressing")
        ok = False
    if ok and d["packets"]:
        log("PASS")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
