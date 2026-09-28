#!/usr/bin/env python3
"""Own the USB-Serial-JTAG port: print UART (monitor) AND pulse DTR/RTS on TCP `reset`.

`espflash reset` opens the port exclusively and kills `espflash monitor`.
This process keeps one fd for both jobs, so a reset does not drop the log session.

  python3 esp-reset-listen.py
  ESPFLASH_PORT=/dev/cu.usbmodem21101 python3 esp-reset-listen.py --port 9473
"""

from __future__ import annotations

import argparse
import array
import fcntl
import glob
import os
import select
import socket
import sys
import termios
import threading
import time

LISTEN_HOST = "0.0.0.0"
LISTEN_PORT = 9473
BAUD = 115200

# Darwin ttycom.h
TIOCMGET = 0x4004746A
TIOCMSET = 0x8004746D
TIOCM_DTR = 0x002
TIOCM_RTS = 0x004


def die(msg: str) -> None:
    print(msg, file=sys.stderr)
    sys.exit(1)


def find_serial() -> str:
    env = os.environ.get("ESPFLASH_PORT")
    if env:
        return env
    ports = sorted(glob.glob("/dev/cu.usbmodem*"))
    if not ports:
        die("no /dev/cu.usbmodem*; set ESPFLASH_PORT")
    if len(ports) > 1:
        die(f"several serial ports: {ports}; set ESPFLASH_PORT")
    return ports[0]


def set_tty(fd: int) -> None:
    t = termios.tcgetattr(fd)
    # iflag
    t[0] = 0
    # oflag
    t[1] = 0
    # cflag: CS8 | CREAD | CLOCAL
    t[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    # lflag
    t[3] = 0
    t[4] = termios.B115200
    t[5] = termios.B115200
    t[6][termios.VMIN] = 0
    t[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, t)
    termios.tcflush(fd, termios.TCIOFLUSH)


def modem_get(fd: int) -> int:
    buf = array.array("i", [0])
    fcntl.ioctl(fd, TIOCMGET, buf, True)
    return buf[0]


def modem_set(fd: int, bits: int) -> None:
    buf = array.array("i", [bits])
    fcntl.ioctl(fd, TIOCMSET, buf)


def set_dtr(fd: int, on: bool) -> None:
    m = modem_get(fd)
    m = m | TIOCM_DTR if on else m & ~TIOCM_DTR
    modem_set(fd, m)


def set_rts(fd: int, on: bool) -> None:
    m = modem_get(fd)
    m = m | TIOCM_RTS if on else m & ~TIOCM_RTS
    modem_set(fd, m)


def reset_after_flash(fd: int) -> None:
    """Same as `espflash monitor` Ctrl+R (`reset_after_flash` for USB-Serial-JTAG).

    Must not use UsbJtagSerialReset (DTR high / IO0 low) — that enters download.
    """
    set_dtr(fd, False)
    time.sleep(0.1)
    set_rts(fd, True)
    set_dtr(fd, False)
    set_rts(fd, True)
    time.sleep(0.1)
    set_rts(fd, False)


def uart_loop(fd: int, stop: threading.Event) -> None:
    while not stop.is_set():
        r, _, _ = select.select([fd], [], [], 0.2)
        if not r:
            continue
        try:
            data = os.read(fd, 4096)
        except OSError:
            if stop.is_set():
                return
            raise
        if not data:
            continue
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()


def handle_client(conn: socket.socket, fd: int, lock: threading.Lock) -> None:
    conn.settimeout(2)
    buf = b""
    try:
        while b"\n" not in buf:
            chunk = conn.recv(64)
            if not chunk:
                break
            buf += chunk
            if len(buf) > 64:
                break
    except OSError:
        conn.close()
        return
    cmd = buf.split(b"\n", 1)[0].strip().decode("ascii", "replace")
    if cmd == "reset":
        ts = time.strftime("%F %T")
        print(f"\n{ts} reset (Ctrl+R / RTS)", file=sys.stderr, flush=True)
        with lock:
            try:
                reset_after_flash(fd)
                conn.sendall(b"ok\n")
            except OSError as e:
                print(f"{ts} reset FAIL {e}", file=sys.stderr, flush=True)
                conn.sendall(b"fail\n")
    conn.close()


def tcp_loop(fd: int, bind: tuple[str, int], stop: threading.Event, lock: threading.Lock) -> None:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(bind)
    srv.listen(4)
    srv.settimeout(0.5)
    print(f"listen {bind[0]}:{bind[1]}  test → 192.168.110.96:{bind[1]}", file=sys.stderr)
    while not stop.is_set():
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            continue
        threading.Thread(target=handle_client, args=(conn, fd, lock), daemon=True).start()
    srv.close()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", type=int, default=LISTEN_PORT)
    ap.add_argument("--serial", default=None)
    args = ap.parse_args()
    path = args.serial or find_serial()
    fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    set_tty(fd)
    print(f"serial {path} {BAUD}  (this IS the monitor — do not also run espflash monitor)", file=sys.stderr)
    stop = threading.Event()
    lock = threading.Lock()
    threading.Thread(target=uart_loop, args=(fd, stop), daemon=True).start()
    threading.Thread(
        target=tcp_loop, args=(fd, (LISTEN_HOST, args.port), stop, lock), daemon=True
    ).start()
    try:
        while True:
            time.sleep(0.5)
    except KeyboardInterrupt:
        stop.set()
        print("\nstop", file=sys.stderr)


if __name__ == "__main__":
    main()
