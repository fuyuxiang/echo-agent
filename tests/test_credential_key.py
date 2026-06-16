import stat

import pytest
from cryptography.fernet import Fernet

from echo_agent.permissions.credential_key import resolve_or_create_key

ENV = "ECHO_AGENT_CREDENTIAL_KEY"


def test_uses_valid_env_key(tmp_path, monkeypatch):
    key = Fernet.generate_key()
    monkeypatch.setenv(ENV, key.decode())
    assert resolve_or_create_key(tmp_path) == key
    assert not (tmp_path / ".credential_key").exists()  # env 命中不落盘


def test_invalid_env_key_raises(tmp_path, monkeypatch):
    monkeypatch.setenv(ENV, "not-a-valid-fernet-key")
    with pytest.raises(ValueError, match="ECHO_AGENT_CREDENTIAL_KEY"):
        resolve_or_create_key(tmp_path)


def test_generates_and_persists_when_absent(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    key = resolve_or_create_key(tmp_path)
    Fernet(key)  # 不抛即合法
    key_file = tmp_path / ".credential_key"
    assert key_file.exists()
    assert key_file.read_bytes() == key
    mode = stat.S_IMODE(key_file.stat().st_mode)
    assert mode == 0o600


def test_reuses_existing_file(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    first = resolve_or_create_key(tmp_path)
    second = resolve_or_create_key(tmp_path)
    assert first == second  # 不重新生成


def test_corrupted_key_file_raises(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    (tmp_path / ".credential_key").write_bytes(b"garbage")
    with pytest.raises(ValueError, match=str(tmp_path)):
        resolve_or_create_key(tmp_path)


def test_empty_env_falls_back_to_file(tmp_path, monkeypatch):
    monkeypatch.setenv(ENV, "")  # 空串当未设置：回退到生成文件
    key = resolve_or_create_key(tmp_path)
    Fernet(key)  # 不抛即合法
    assert (tmp_path / ".credential_key").exists()
