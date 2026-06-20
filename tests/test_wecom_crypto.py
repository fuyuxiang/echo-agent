import base64
import os
import struct

import pytest

from echo_agent.channels.wecom_crypto import decrypt_message, verify_signature


def _encrypt(aes_key_b64: str, corp_id: str, plaintext: str) -> str:
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    key = base64.b64decode(aes_key_b64 + "=")
    iv = key[:16]
    rand = os.urandom(16)
    msg = plaintext.encode()
    raw = rand + struct.pack(">I", len(msg)) + msg + corp_id.encode()
    pad = 32 - (len(raw) % 32)
    raw += bytes([pad]) * pad
    enc = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    return base64.b64encode(enc.update(raw) + enc.finalize()).decode()


def test_decrypt_round_trip():
    aes_key = base64.b64encode(os.urandom(32)).decode().rstrip("=")
    corp_id = "wwcorp123"
    cipher = _encrypt(aes_key, corp_id, "<xml><Content>hi</Content></xml>")
    assert decrypt_message(aes_key, corp_id, cipher) == "<xml><Content>hi</Content></xml>"


def test_decrypt_rejects_wrong_corp_id():
    aes_key = base64.b64encode(os.urandom(32)).decode().rstrip("=")
    cipher = _encrypt(aes_key, "rightcorp", "<xml/>")
    with pytest.raises(ValueError):
        decrypt_message(aes_key, "wrongcorp", cipher)


def test_verify_signature_sorts_four_tuple():
    sig = verify_signature("tok", "100", "nonce", "encblob")
    import hashlib
    expected = hashlib.sha1("".join(sorted(["tok", "100", "nonce", "encblob"])).encode()).hexdigest()
    assert sig == expected


# ── channel-level: plaintext mode must not regress; encrypted mode verifies sig ──

def _make_channel(encoding_aes_key: str, corp_id: str = "corp123", token: str = "mytoken"):
    from unittest.mock import MagicMock
    from echo_agent.channels.wecom import WeComChannel

    config = MagicMock()
    config.corp_id = corp_id
    config.agent_id = "1000001"
    config.secret = "sec"
    config.token = token
    config.encoding_aes_key = encoding_aes_key
    config.webhook_path = "/wecom"
    config.host = "0.0.0.0"
    config.port = 8084
    config.allow_from = []
    bus = MagicMock()
    return WeComChannel(config, bus)


@pytest.mark.asyncio
async def test_verify_plaintext_mode_unchanged():
    """Plaintext mode (no aes key) keeps echoing echostr on valid 3-tuple sig."""
    import hashlib
    from aiohttp.test_utils import make_mocked_request

    ch = _make_channel("")
    ts, nonce, echo = "1234567890", "nonce123", "echo_back"
    sig = hashlib.sha1("".join(sorted(["mytoken", ts, nonce])).encode()).hexdigest()
    req = make_mocked_request(
        "GET", f"/wecom?msg_signature={sig}&timestamp={ts}&nonce={nonce}&echostr={echo}"
    )
    resp = await ch._verify(req)
    assert resp.status == 200
    assert resp.text == echo


@pytest.mark.asyncio
async def test_verify_encrypted_mode_round_trip():
    """Encrypted mode verifies the 4-tuple sig then decrypts echostr."""
    from aiohttp.test_utils import make_mocked_request
    from urllib.parse import quote

    aes_key = base64.b64encode(os.urandom(32)).decode().rstrip("=")
    corp_id = "corp123"
    ch = _make_channel(aes_key, corp_id=corp_id)
    echostr = _encrypt(aes_key, corp_id, "plain_echo")
    ts, nonce = "100", "n1"
    sig = verify_signature("mytoken", ts, nonce, echostr)
    req = make_mocked_request(
        "GET",
        f"/wecom?msg_signature={sig}&timestamp={ts}&nonce={nonce}&echostr={quote(echostr)}",
    )
    resp = await ch._verify(req)
    assert resp.status == 200
    assert resp.text == "plain_echo"


@pytest.mark.asyncio
async def test_verify_encrypted_mode_rejects_bad_signature():
    """Encrypted mode returns 403 when msg_signature does not match."""
    from aiohttp.test_utils import make_mocked_request

    aes_key = base64.b64encode(os.urandom(32)).decode().rstrip("=")
    ch = _make_channel(aes_key, corp_id="corp123")
    echostr = _encrypt(aes_key, "corp123", "x")
    req = make_mocked_request(
        "GET", f"/wecom?msg_signature=deadbeef&timestamp=100&nonce=n1&echostr={echostr}"
    )
    resp = await ch._verify(req)
    assert resp.status == 403

