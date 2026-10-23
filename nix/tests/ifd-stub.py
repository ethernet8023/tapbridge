"""Stands in for the tapbridge app in the NixOS VM test: a unix socket
speaking the IFD protocol (crates/host/src/ifd.rs) that reports a card and
answers every APDU with 9000. The app itself needs a display."""
import os
import socket
import struct
import sys

ATR = bytes.fromhex("3B8F8001804F0CA0000003060300030000000068")
path = sys.argv[1]
try:
    os.unlink(path)
except FileNotFoundError:
    pass
server = socket.socket(socket.AF_UNIX)
server.bind(path)
os.chmod(path, 0o660)  # as the app does: pcscd reaches it through the directory group
server.listen(8)


def read_exact(conn, n):
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            raise EOFError
        data += chunk
    return data


while True:
    conn, _ = server.accept()
    try:
        body = read_exact(conn, struct.unpack(">I", read_exact(conn, 4))[0])
        cmd = body[0]
        print("cmd", cmd, len(body) - 1, flush=True)
        if cmd in (2, 3):  # ATR, power on
            reply = b"\x00" + ATR
        elif cmd == 5:  # transmit
            reply = b"\x00" + bytes.fromhex("9000")
        else:  # presence, power off
            reply = b"\x00"
        conn.sendall(struct.pack(">I", len(reply)) + reply)
    except (EOFError, OSError) as error:
        print("error", repr(error), flush=True)
    finally:
        conn.close()
