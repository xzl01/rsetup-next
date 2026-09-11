#!/usr/bin/env python3
"""
Drive the rsetup-next TUI on a remote SBC over SSH inside a local PTY,
capture the raw ANSI byte stream, and save it for screenshot rendering.

Usage:
  capture-tui-remote.py <host> <output_raw_file> <locale> <cols> <rows> [binary_path]
"""

import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time

SSH_KEY = os.path.expanduser("~/.ssh/id_ed25519")
SSH_OPTS = [
    "-F", "/dev/null",
    "-i", SSH_KEY,
    "-o", "StrictHostKeyChecking=no",
    "-o", "UserKnownHostsFile=/dev/null",
    "-t", "-t",
]


def capture(host, out_path, locale, cols=100, rows=30, binary="/tmp/rsetup-next-test",
            settle_seconds=3.5):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    remote_cmd = f"LANG={locale} LC_ALL={locale} {binary} tui"
    cmd = ["ssh", *SSH_OPTS, f"root@{host}", remote_cmd]

    proc = subprocess.Popen(cmd, stdin=slave, stdout=slave, stderr=slave, close_fds=True)
    os.close(slave)

    buf = bytearray()
    deadline = time.time() + settle_seconds
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(master, 8192)
                if chunk:
                    buf.extend(chunk)
            except OSError:
                break

    # Graceful quit so the remote TUI restores the terminal.
    os.write(master, b"q")
    time.sleep(0.5)

    while True:
        ready, _, _ = select.select([master], [], [], 0.2)
        if not ready:
            break
        try:
            chunk = os.read(master, 8192)
            if not chunk:
                break
            buf.extend(chunk)
        except OSError:
            break

    os.close(master)
    try:
        proc.wait(timeout=3)
    except Exception:
        proc.terminate()

    with open(out_path, "wb") as handle:
        handle.write(buf)
    return len(buf)


def main():
    if len(sys.argv) < 5:
        print(__doc__)
        return 1
    host = sys.argv[1]
    out_path = sys.argv[2]
    locale = sys.argv[3]
    cols = int(sys.argv[4])
    rows = int(sys.argv[5])
    binary = sys.argv[6] if len(sys.argv) > 6 else "/tmp/rsetup-next-test"
    size = capture(host, out_path, locale, cols, rows, binary)
    print(f"{host}: captured {size} bytes -> {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
