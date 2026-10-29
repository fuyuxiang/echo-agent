from types import SimpleNamespace

import pytest

from echo_agent.cli import health as doctor
from echo_agent.gateway.health import GatewayHealthProvider
from echo_agent.storage import health


@pytest.mark.parametrize("available,status", [
    (0, "fail"), (1024**2, "fail"), (health.MIN_FREE_BYTES - 1, "fail"),
    (health.MIN_FREE_BYTES, "warn"), (128 * 1024**2, "warn"),
    (health.LOW_FREE_BYTES, "ok"), (1024**3, "ok"),
])
def test_doctor_checks_actual_data_filesystem(tmp_path, monkeypatch, available, status):
    seen = []

    def free(path):
        seen.append(path)
        return available

    monkeypatch.setattr(doctor, "filesystem_free_bytes", free)
    database = tmp_path / "separate-volume/state.db"
    result = doctor.check_storage({"workspace": str(tmp_path), "storage": {"database_path": str(database)}})
    assert result["status"] == status
    assert seen == [database.parent]


def test_disk_probe_uses_available_blocks_on_nearest_existing_ancestor(tmp_path, monkeypatch):
    seen = []

    def usage(path):
        seen.append(path)
        # total/used subtraction would show 2 GiB, but only 1 MiB is available
        # after filesystem reservations. Always use .free.
        return SimpleNamespace(total=40 * 1024**3, used=38 * 1024**3, free=1024**2)

    monkeypatch.setattr(health.shutil, "disk_usage", usage)
    assert health.filesystem_free_bytes(tmp_path / "new/data/database") == 1024**2
    assert seen == [tmp_path]


def test_ready_rejects_connected_database_on_full_disk_without_leaking_path(tmp_path, monkeypatch):
    storage = SimpleNamespace(is_connected=True, _db_path=tmp_path / "private/state.db")
    agent = SimpleNamespace(provider=SimpleNamespace(is_stub=False), _storage=storage, is_running=True)
    gateway = SimpleNamespace(_agent_loop=agent, is_running=True, _bus=SimpleNamespace(_running=True))
    monkeypatch.setattr(health, "filesystem_free_bytes", lambda _: 0)
    result = GatewayHealthProvider(gateway).readiness()
    assert not result["ready"]
    assert not result["checks"]["storage"]["ok"]
    assert str(tmp_path) not in str(result)


@pytest.mark.parametrize("probe", ["doctor", "ready"])
def test_storage_probe_follows_database_symlink(tmp_path, monkeypatch, probe):
    data = tmp_path / "other-volume"
    data.mkdir()
    target = data / "state.db"
    target.touch()
    database = tmp_path / "state.db"
    database.symlink_to(target)
    seen = []

    def free(path):
        seen.append(path)
        return 0

    if probe == "doctor":
        monkeypatch.setattr(doctor, "filesystem_free_bytes", free)
        result = doctor.check_storage({"storage": {"database_path": str(database)}})
        assert result["status"] == "fail"
    else:
        monkeypatch.setattr(health, "filesystem_free_bytes", free)
        storage = SimpleNamespace(is_connected=True, _db_path=database)
        agent = SimpleNamespace(provider=SimpleNamespace(is_stub=False), _storage=storage, is_running=True)
        gateway = SimpleNamespace(_agent_loop=agent, is_running=True, _bus=SimpleNamespace(_running=True))
        assert not GatewayHealthProvider(gateway).readiness()["ready"]
    assert seen == [target.parent]
