import errno
import json

import pytest

from echo_agent.channels import weixin as wx


@pytest.mark.parametrize("state", ["tokens", "cursor"])
def test_disk_full_keeps_last_good_weixin_state(tmp_path, monkeypatch, state):
    account = "test@im.bot"
    suffix = "context-tokens" if state == "tokens" else "sync"
    target = tmp_path / f"{account}.{suffix}.json"
    original = {"user": "original-token"} if state == "tokens" else {"sync_buf": "old-cursor"}
    target.write_text(json.dumps(original))

    def full_disk(*args, **kwargs):
        raise OSError(errno.ENOSPC, "No space left on device")

    monkeypatch.setattr(wx.os, "fsync", full_disk)
    if state == "tokens":
        store = wx._ContextTokenStore(tmp_path)
        store.restore(account)
        store.set(account, "user", "new-token")
        assert store.get(account, "user") == "new-token"
    else:
        wx._save_sync_buf(tmp_path, account, "new-cursor")
    assert json.loads(target.read_text()) == original
    assert list(tmp_path.iterdir()) == [target]


def test_atomic_state_replaces_complete_json_and_uses_private_permissions(tmp_path):
    target = tmp_path / "state.json"
    wx._atomic_write_json(target, {"用户": "token"})
    assert json.loads(target.read_text()) == {"用户": "token"}
    assert target.stat().st_mode & 0o077 == 0


def test_corrupt_primary_recovers_last_complete_backup(tmp_path):
    store = wx._ContextTokenStore(tmp_path)
    store.set("account", "user", "context")
    (tmp_path / "account.context-tokens.json").write_text("")
    restarted = wx._ContextTokenStore(tmp_path)
    restarted.restore("account")
    assert restarted.get("account", "user") == "context"
    assert json.loads((tmp_path / "account.context-tokens.json").read_text()) == {"user": "context"}
    assert (tmp_path / "account.context-tokens.json.bak").stat().st_mode & 0o077 == 0
