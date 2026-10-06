#!/usr/bin/env python3
"""Independent generator of caBLEv2 test vectors for crates/hybrid.

Everything cross-checked against an independent implementation:
- HKDF-SHA256 via python hashlib/hmac (RFC 5869, written from scratch)
- AES-256-ECB single block via `openssl enc -aes-256-ecb -nopad`
- SHA-256 domain hashing via hashlib

Emits JSON to stdout.
"""
import hashlib
import hmac
import json
import subprocess

def hkdf_sha256(ikm: bytes, salt: bytes | None, info: bytes, length: int) -> bytes:
    if salt is None or len(salt) == 0:
        salt = b"\x00" * 32
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    okm = b""
    t = b""
    counter = 1
    while len(okm) < length:
        t = hmac.new(prk, t + info + bytes([counter]), hashlib.sha256).digest()
        okm += t
        counter += 1
    return okm[:length]

def derive(secret: bytes, salt: bytes | None, purpose: int, out_len: int) -> bytes:
    assert purpose < 0x100
    info = bytes([purpose, 0, 0, 0])  # 32-bit little-endian
    return hkdf_sha256(secret, salt, info, out_len)

def openssl_aes256_ecb(key: bytes, block: bytes, decrypt: bool) -> bytes:
    assert len(key) == 32 and len(block) == 16
    args = ["openssl", "enc", "-aes-256-ecb", "-nopad", "-K", key.hex()]
    if decrypt:
        args.append("-d")
    out = subprocess.run(args, input=block, capture_output=True, check=True).stdout
    assert len(out) == 16, out
    return out

def make_advert(eid_key: bytes, plaintext16: bytes) -> bytes:
    ct = openssl_aes256_ecb(eid_key[:32], plaintext16, decrypt=False)
    tag = hmac.new(eid_key[32:], ct, hashlib.sha256).digest()[:4]
    return ct + tag

def decode_tunnel_domain(encoded: int) -> str:
    if encoded < 256:
        table = ["cable.ua5v.com", "cable.auth.com"]
        return table[encoded]  # caller bounds-checks
    sha_input = b"caBLEv2 tunnel server domain" + bytes([encoded & 0xFF, (encoded >> 8) & 0xFF, 0])
    digest = hashlib.sha256(sha_input).digest()
    v = int.from_bytes(digest[:8], "little")
    tld_index = v & 3
    v >>= 2
    ret = "cable."
    base32 = "abcdefghijklmnopqrstuvwxyz234567"
    while v != 0:
        ret += base32[v & 31]
        v >>= 5
    tlds = [".com", ".org", ".net", ".info"]
    return ret + tlds[tld_index & 3]

def digit_encode(d: bytes) -> str:
    chunk_size, chunk_digits = 7, 17
    zeros = "0" * chunk_digits
    ret = ""
    while len(d) >= chunk_size:
        v = int.from_bytes(d[:chunk_size], "little")
        s = str(v)
        ret += zeros[: chunk_digits - len(s)] + s
        d = d[chunk_size:]
    if len(d) != 0:
        partial = {1: 3, 2: 5, 3: 8, 4: 10, 5: 13, 6: 15}[len(d)]
        v = int.from_bytes(d, "little")
        s = str(v)
        ret += zeros[: partial - len(s)] + s
    return ret

# ---- vectors ----
qr_secret = bytes(range(16))  # 00..0f
eid_key = derive(qr_secret, None, 1, 64)
tunnel_id = derive(qr_secret, None, 2, 16)

advert_plain = bytes([0]) + bytes(range(1, 11)) + bytes([0xAA, 0xBB, 0xCC]) + bytes([0x00, 0x01])
assert len(advert_plain) == 16
advert_service_data = make_advert(eid_key, advert_plain)

other_secret = bytes([0x77] * 16)
eid_key2 = derive(other_secret, None, 1, 64)
tunnel_id2 = derive(other_secret, None, 2, 16)
psk2 = derive(other_secret, advert_plain, 3, 32)

# state-assisted eid key: parent = 32-byte link secret (CTAP2.2 linkData linkSecret is [32]byte), salt = client nonce
client_nonce = bytes([0x42] * 16)
eid_key_sa = derive(bytes(32), client_nonce, 1, 64)
sa_plain = bytes(16)  # flags 0 + 15 zero nonce bytes

# QR CBOR: map {0: bstr33, 1: bstr16, 2: 2, 3: 0x1a00000000+1234567890, 4: true, 5: "mc"}
pub33 = bytes([0x02]) + bytes(range(1, 33))
epoch = 1234567890
cbor = bytes([0xA6, 0x00, 0x58, 0x21]) + pub33
cbor += bytes([0x01, 0x50]) + qr_secret
cbor += bytes([0x02, 0x02])
cbor += bytes([0x03, 0x1A]) + epoch.to_bytes(4, "big")
cbor += bytes([0x04, 0xF5])
cbor += bytes([0x05, 0x62]) + b"mc"
qr_string = "FIDO:/" + digit_encode(cbor)

# client payload: {1: bstr8 linkid, 2: bstr16 nonce, 3: "ga"}
link_id = bytes([0x10] * 8)
client_payload = bytes([0xA3, 0x01, 0x48]) + link_id
client_payload += bytes([0x02, 0x50]) + client_nonce
client_payload += bytes([0x03, 0x62]) + b"ga"

out = {
    "qr_secret_hex": qr_secret.hex(),
    "eid_key_hex": eid_key.hex(),
    "tunnel_id_hex": tunnel_id.hex(),
    "advert_plain_hex": advert_plain.hex(),
    "advert_service_data_hex": advert_service_data.hex(),
    "other_secret_hex": other_secret.hex(),
    "eid_key2_hex": eid_key2.hex(),
    "tunnel_id2_hex": tunnel_id2.hex(),
    "psk2_hex": psk2.hex(),
    "client_nonce_hex": client_nonce.hex(),
    "eid_key_sa_hex": eid_key_sa.hex(),
    "sa_plain_hex": sa_plain.hex(),
    "domain_256": decode_tunnel_domain(256),
    "domain_257": decode_tunnel_domain(257),
    "domain_300": decode_tunnel_domain(300),
    "domain_assigned_0": decode_tunnel_domain(0),
    "domain_assigned_1": decode_tunnel_domain(1),
    "qr_pub33_hex": pub33.hex(),
    "qr_cbor_hex": cbor.hex(),
    "qr_string": qr_string,
    "link_id_hex": link_id.hex(),
    "client_payload_hex": client_payload.hex(),
}
import sys
json.dump(out, sys.stdout, indent=1)
print()
