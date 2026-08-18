"""WeCom (Enterprise WeChat) encrypted-callback crypto helpers.

Implements the standard 企业微信 callback scheme: AES-256-CBC with a
PKCS7-padded payload of random(16) + len(4, big-endian) + msg + receive_id,
plus the sha1 four-tuple message signature.
"""
from __future__ import annotations

import base64
import hashlib
import struct

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes


def verify_signature(token: str, timestamp: str, nonce: str, *extra: str) -> str:
    """Return the expected sha1 signature over the sorted tuple."""
    items = sorted([token, timestamp, nonce, *extra])
    return hashlib.sha1("".join(items).encode()).hexdigest()


def decrypt_message(encoding_aes_key: str, corp_id: str, encrypt_b64: str) -> str:
    """Decrypt a WeCom <Encrypt> blob; return the plaintext XML.

    Raises ValueError on padding/format errors or receive_id mismatch.
    """
    key = base64.b64decode(encoding_aes_key + "=")
    if len(key) != 32:
        raise ValueError("encoding_aes_key must decode to 32 bytes")
    iv = key[:16]
    ciphertext = base64.b64decode(encrypt_b64)
    decryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).decryptor()
    raw = decryptor.update(ciphertext) + decryptor.finalize()
    if not raw:
        raise ValueError("empty plaintext")
    pad = raw[-1]
    if pad < 1 or pad > 32:
        raise ValueError("invalid padding")
    raw = raw[:-pad]
    # random(16) + msg_len(4) + msg + receive_id
    if len(raw) < 20:
        raise ValueError("plaintext too short for header")
    try:
        msg_len = struct.unpack(">I", raw[16:20])[0]
    except struct.error as e:
        raise ValueError(f"malformed length header: {e}") from e
    msg = raw[20:20 + msg_len].decode()
    receive_id = raw[20 + msg_len:].decode()
    if receive_id != corp_id:
        raise ValueError("receive_id mismatch (possible forgery)")
    return msg
