#!/usr/bin/env python3
"""End-to-end PIV probe through the running tapbridge daemon.

Talks the IFD shim protocol (crates/host/src/ifd.rs) straight to the daemon's
socket, skipping macOS PC/SC: power on, SELECT the PIV applet, read the CHUID
and the slot 9A (authentication) certificate. Proves Mac -> daemon -> phone ->
NFC -> card for PIV without needing the virtual reader to show up in PC/SC.

Usage: open tapbridge on the phone, then run
    scripts/piv-probe.py            # waits up to 60 s for a card on the phone
Read-only: never verifies a PIN, generates keys or writes.
"""
import socket
import struct
import sys
import time

SOCKET = "/var/tmp/tapbridge-ifd.sock"
CMD_PRESENCE, CMD_ATR, CMD_POWER_ON, CMD_POWER_OFF, CMD_TRANSMIT = 1, 2, 3, 4, 5
STATUS = {0: "OK", 1: "no card", 2: "no session", 3: "busy", 4: "comm error", 5: "bad request"}

PIV_AID = bytes.fromhex("A000000308")
# Control: no card has this applet. A card that answers 9000 here answers
# 9000 to every SELECT, so its "PIV present" means nothing.
BOGUS_AID = bytes.fromhex("A0000000DEADBEEF01")
FIDO_AID = bytes.fromhex("A0000006472F0001")


def call(cmd, payload=b""):
    body = bytes([cmd]) + payload
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(30)
        sock.connect(SOCKET)
        sock.sendall(struct.pack(">I", len(body)) + body)
        (length,) = struct.unpack(">I", recv_exact(sock, 4))
        reply = recv_exact(sock, length)
    return reply[0], reply[1:]


def recv_exact(sock, n):
    out = b""
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            raise ConnectionError("daemon closed the socket")
        out += chunk
    return out


def apdu(hexstr_or_bytes):
    """Sends one APDU, follows 61xx GET RESPONSE chains; returns (data, sw)."""
    command = bytes.fromhex(hexstr_or_bytes) if isinstance(hexstr_or_bytes, str) else hexstr_or_bytes
    data = b""
    while True:
        status, reply = call(CMD_TRANSMIT, command)
        if status != 0:
            raise RuntimeError(f"transmit failed: {STATUS.get(status, status)}")
        if len(reply) < 2:
            raise RuntimeError("short response")
        data += reply[:-2]
        sw = reply[-2:]
        if sw[0] == 0x61:
            command = bytes([0x00, 0xC0, 0x00, 0x00, sw[1]])
            continue
        return data, sw.hex().upper()


def select(aid):
    return apdu(bytes([0x00, 0xA4, 0x04, 0x00, len(aid)]) + aid + b"\x00")


def get_data(tag):
    # PIV GET DATA: 00 CB 3F FF Lc 5C len tag 00
    body = bytes([0x5C, len(tag)]) + tag
    return apdu(bytes([0x00, 0xCB, 0x3F, 0xFF, len(body)]) + body + b"\x00")


def tlv(data):
    """Minimal BER-TLV walker: yields (tag_bytes, value)."""
    i = 0
    while i < len(data):
        start = i
        i += 1
        if data[start] & 0x1F == 0x1F:
            while data[i] & 0x80:
                i += 1
            i += 1
        tag = data[start:i]
        length = data[i]
        i += 1
        if length & 0x80:
            count = length & 0x7F
            length = int.from_bytes(data[i:i + count], "big")
            i += count
        yield tag, data[i:i + length]
        i += length


def main():
    try:
        call(CMD_PRESENCE)
    except (FileNotFoundError, ConnectionRefusedError, PermissionError) as error:
        sys.exit(f"tapbridge daemon not reachable at {SOCKET}: {error}")

    print("Waiting for a card: open tapbridge on the phone and hold your key to it...")
    status, atr = call(CMD_POWER_ON)  # also wakes the phone when no session is live
    deadline = time.time() + 60
    while status != 0 and time.time() < deadline:
        time.sleep(1)
        status, atr = call(CMD_POWER_ON)
    if status != 0:
        sys.exit(f"no card after 60 s ({STATUS.get(status, status)})")
    print(f"ATR        {atr.hex().upper()}")

    data, sw = select(FIDO_AID)
    print(f"FIDO applet  SW {sw}" + ("  (present)" if sw == "9000" else ""))

    data, sw = select(BOGUS_AID)
    print(f"bogus applet SW {sw}" + ("  <- card says 9000 to ANY select" if sw == "9000" else "  (correctly rejected)"))

    data, sw = select(PIV_AID)
    print(f"PIV SELECT   response {data.hex().upper() or '(empty)'}")
    if sw == "9000" and not data:
        print("             a real PIV applet returns an application template (61 4F ...) here")
    if sw != "9000":
        print(f"PIV applet   SW {sw}  -> this card has no PIV applet (6A82 = not found)")
        sys.exit(2)
    print(f"PIV applet   SW 9000  (present)")
    for tag, value in tlv(data):
        if tag == b"\x61":
            for inner, field in tlv(value):
                if inner == b"\x50":
                    print(f"  label    {field.decode(errors='replace')}")
                elif inner == b"\x4F":
                    print(f"  AID      {field.hex().upper()}")

    data, sw = get_data(bytes.fromhex("5FC102"))
    print(f"CHUID        SW {sw}" + (f"  ({len(data)} bytes)" if sw == "9000" else ""))

    for slot, tag in (("9A auth", "5FC105"), ("9C sign", "5FC10A"), ("9D keymgmt", "5FC10B"), ("9E cardauth", "5FC101")):
        data, sw = get_data(bytes.fromhex(tag))
        if sw == "9000":
            cert = next((v for t, v in tlv(data) if t == b"\x53"), data)
            inner = next((v for t, v in tlv(cert) if t == b"\x70"), b"")
            print(f"cert {slot:<10} present ({len(inner)} bytes DER)")
        else:
            print(f"cert {slot:<10} SW {sw}  (empty slot)" if sw == "6A82" else f"cert {slot:<10} SW {sw}")
    call(CMD_POWER_OFF)


if __name__ == "__main__":
    main()
