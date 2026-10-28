import stat

import pytest
from cryptography.fernet import Fernet

from echo_agent.permissions.credential_key import resolve_or_create_key
from echo_agent.permissions.manager import CredentialManager

ENV = "ECHO_AGENT_CREDENTIAL_KEY"


def test_uses_valid_env_key(tmp_path, monkeypatch):
    key = Fernet.generate_key()
    monkeypatch.setenv(ENV, key.decode())
    assert resolve_or_create_key(tmp_path / ".credential_key") == key
    assert not (tmp_path / ".credential_key").exists()  # env 命中不落盘


def test_invalid_env_key_raises(tmp_path, monkeypatch):
    monkeypatch.setenv(ENV, "not-a-valid-fernet-key")
    with pytest.raises(ValueError, match="ECHO_AGENT_CREDENTIAL_KEY"):
        resolve_or_create_key(tmp_path / ".credential_key")


def test_generates_and_persists_when_absent(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    key = resolve_or_create_key(tmp_path / ".credential_key")
    Fernet(key)  # 不抛即合法
    key_file = tmp_path / ".credential_key"
    assert key_file.exists()
    assert key_file.read_bytes() == key
    mode = stat.S_IMODE(key_file.stat().st_mode)
    assert mode == 0o600


def test_reuses_existing_file(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    first = resolve_or_create_key(tmp_path / ".credential_key")
    second = resolve_or_create_key(tmp_path / ".credential_key")
    assert first == second  # 不重新生成


def test_corrupted_key_file_raises(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    (tmp_path / ".credential_key").write_bytes(b"garbage")
    with pytest.raises(ValueError, match=str(tmp_path)):
        resolve_or_create_key(tmp_path / ".credential_key")


def test_empty_env_falls_back_to_file(tmp_path, monkeypatch):
    monkeypatch.setenv(ENV, "")  # 空串当未设置：回退到生成文件
    key = resolve_or_create_key(tmp_path / ".credential_key")
    Fernet(key)  # 不抛即合法
    assert (tmp_path / ".credential_key").exists()


def test_manager_roundtrip_fernet(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    store = tmp_path / "data" / "credentials.json"
    key_path = tmp_path / ".credential_key"
    mgr = CredentialManager(store_path=store, key_path=key_path)
    mgr.store("openai", "sk-secret-123")

    raw = store.read_text(encoding="utf-8")
    assert '"encoding": "fernet"' in raw
    assert "sk-secret-123" not in raw

    mgr2 = CredentialManager(store_path=store, key_path=key_path)
    assert mgr2.get("openai") == "sk-secret-123"


def test_manager_rejects_undecryptable_legacy(tmp_path, monkeypatch):
    """异密钥加密的密文与新 key 不兼容时，必须清晰报错而非静默吞。"""
    monkeypatch.delenv(ENV, raising=False)
    import json
    store = tmp_path / "data" / "credentials.json"
    store.parent.mkdir(parents=True, exist_ok=True)
    other = Fernet(Fernet.generate_key())
    store.write_text(json.dumps({
        "format": "echo-agent-credentials-v2",
        "credentials": [{
            "id": "x1", "name": "legacy", "tool_scope": "*",
            "value_hash": "", "created_at": "", "rotated_at": "",
            "encoding": "fernet", "value_enc": other.encrypt(b"old").decode(),
        }],
    }), encoding="utf-8")
    with pytest.raises(RuntimeError, match="凭证密钥"):
        CredentialManager(store_path=store, key_path=tmp_path / ".credential_key")
