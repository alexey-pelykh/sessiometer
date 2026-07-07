#!/usr/bin/env python3
# Copyright (c) 2026 Oleksii PELYKH
# SPDX-License-Identifier: MIT
#
# THROWAWAY SPIKE stub (issue #321) — a minimal AF_UNIX server that vends the daemon's
# byte-exact `snapshot`/`heartbeat` `watch` frames so the Swift client plumbing
# (`watch_spike.swift`) can be validated WITHOUT the live daemon (and without touching any
# credential — the #209 boundary). It stands in for `src/daemon/socket.rs::serve_watch`.
#
# The frames below are copied BYTE-FOR-BYTE from `apps/menubar/Tests/Fixtures.swift`
# (`snapshotBasic` / `heartbeatBasic`), which are themselves byte-exact daemon encoder output
# (see that file's header). Keep them in sync with the fixtures, not re-derived.
#
# Flags:
#   --socket PATH   bind here (under the repo .tmp/ or worktree — never /tmp)
#   --serve N       accept N connections then exit (default 2: the spike's sync + async runs)
#   --chunked       split the snapshot across two writes (first WITHOUT the newline) with a
#                   gap, forcing the client to accumulate a PARTIAL read before the \n boundary
#   --delay SEC     sleep SEC before sending the first byte (lets the client's SIGALRM fire
#                   mid-read → exercises its EINTR retry)
#
# Not part of any build; run directly:  python3 stub_daemon.py --socket .tmp/k.sock

import argparse
import os
import socket
import sys
import time

# Byte-exact from apps/menubar/Tests/Fixtures.swift § snapshotBasic / heartbeatBasic.
SNAPSHOT_BASIC = (
    '{"type":"snapshot","schema_version":{"major":1,"minor":0},"generated_at":42,'
    '"accounts":[{"label":"work","active":true,"enabled":true,"quarantined":false,'
    '"recovering":false,"session_pct":60,"weekly_pct":10,"session_resets_at":null,'
    '"weekly_resets_at":null,"weekly_exhausted":false,"access_expires_at":null,'
    '"refresh_health":null,"auth":"healthy"}],"next_swap":null,"refresh_enabled":false}'
)
HEARTBEAT_BASIC = (
    '{"type":"heartbeat","generated_at":42,"schema_version":{"major":1,"minor":0}}'
)


def serve_one(conn, chunked, delay):
    # Read the client's newline-terminated `{"cmd":"watch"}` request.
    buf = b""
    conn.settimeout(5)
    while b"\n" not in buf:
        b = conn.recv(4096)
        if not b:
            return
        buf += b
    sys.stderr.write(f"stub: received request: {buf!r}\n")

    if delay:
        time.sleep(delay)  # let the client's SIGALRM interrupt its blocking read (EINTR path)

    frame = (SNAPSHOT_BASIC + "\n").encode()
    if chunked:
        # Split so the FIRST write carries no newline: the client must accumulate across two
        # read()s before it can extract the line.
        cut = len(frame) // 2
        conn.sendall(frame[:cut])
        sys.stderr.write(f"stub: sent partial chunk 1 ({cut} bytes, no newline)\n")
        time.sleep(0.05)
        conn.sendall(frame[cut:])
        sys.stderr.write(f"stub: sent chunk 2 ({len(frame) - cut} bytes, with newline)\n")
    else:
        conn.sendall(frame)
        sys.stderr.write(f"stub: sent snapshot frame ({len(frame)} bytes)\n")

    # A follow-up heartbeat, then hold briefly so a slow async consumer still reads frame #1.
    time.sleep(0.05)
    try:
        conn.sendall((HEARTBEAT_BASIC + "\n").encode())
    except OSError:
        pass  # client already closed after its first frame — expected
    time.sleep(0.2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", required=True)
    ap.add_argument("--serve", type=int, default=2)
    ap.add_argument("--chunked", action="store_true")
    ap.add_argument("--delay", type=float, default=0.0)
    args = ap.parse_args()

    try:
        os.unlink(args.socket)  # remove a stale socket so bind() doesn't EADDRINUSE
    except FileNotFoundError:
        pass

    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(args.socket)
    os.chmod(args.socket, 0o600)  # mirror the daemon's 0600 control socket
    srv.listen(4)
    srv.settimeout(10)
    sys.stderr.write(f"stub: listening on {args.socket} (serve={args.serve})\n")

    served = 0
    while served < args.serve:
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            sys.stderr.write("stub: accept timed out — exiting\n")
            break
        with conn:
            serve_one(conn, args.chunked, args.delay)
        served += 1

    srv.close()
    try:
        os.unlink(args.socket)
    except FileNotFoundError:
        pass
    sys.stderr.write(f"stub: served {served} connection(s) — done\n")


if __name__ == "__main__":
    main()
