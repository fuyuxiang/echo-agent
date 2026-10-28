"""Credential corruption and failed writes must never erase stored secrets."""
import os

import pytest

from echo_agent.permissions.manager import CredentialManager
from echo_agent.storage.errors import CorruptData


@pytest.mark.parametrize('payload', ['{broken', '[1]', '{"credentials":[{"id":"a","name":"good","_value":"secret"},{}]}'])
def test_corrupt_credentials_reject_loading_without_rewriting(tmp_path, payload):
    path = tmp_path / 'data' / 'credentials.json'
    path.parent.mkdir()
    path.write_text(payload)
    with pytest.raises(CorruptData):
        CredentialManager(path)
    assert path.read_text() == payload


def test_credentials_save_is_atomic_and_permissions_are_private(tmp_path, monkeypatch):
    path = tmp_path / 'data' / 'credentials.json'
    manager = CredentialManager(path)
    manager.store('token', 'old')
    original = path.read_bytes()
    def broken_replace(*args):
        raise OSError('disk error')
    monkeypatch.setattr(os, 'replace', broken_replace)
    with pytest.raises(OSError, match='disk error'):
        manager.rotate('token', 'new')
    assert path.read_bytes() == original
    assert manager.get('token') == 'old'
    assert path.stat().st_mode & 0o077 == 0
    assert list(path.parent.iterdir()) == [path]
