"""Tests for echo_agent.runtime_lock — workspace single-instance lock.

Guards the fix for the duplicate-reply bug: a background service and a
foreground ``run`` against the same workspace must not both consume channels.
"""

from pathlib import Path

import pytest

from echo_agent.runtime_lock import (
    LOCK_FILENAME,
    InstanceLock,
    InstanceLockError,
    acquire_instance_lock,
)


class TestAcquireInstanceLock:
    def test_lock_file_lives_under_data_dir(self, tmp_path: Path):
        lock = acquire_instance_lock(tmp_path, role="run")
        try:
            assert lock.path == tmp_path / "data" / LOCK_FILENAME
            assert lock.path.exists()
        finally:
            lock.release()

    def test_records_pid_for_diagnostics(self, tmp_path: Path):
        import os

        lock = acquire_instance_lock(tmp_path, role="gateway")
        try:
            content = lock.path.read_text(encoding="utf-8")
            assert f"pid={os.getpid()}" in content
            assert "role=gateway" in content
        finally:
            lock.release()

    def test_second_acquire_same_workspace_conflicts(self, tmp_path: Path):
        lock = acquire_instance_lock(tmp_path, role="run")
        try:
            with pytest.raises(InstanceLockError) as exc:
                acquire_instance_lock(tmp_path, role="gateway")
            # The error must carry an actionable, user-facing hint.
            assert "echo-agent cli" in exc.value.message
            assert "--force" in exc.value.message
        finally:
            lock.release()

    def test_different_workspaces_run_in_parallel(self, tmp_path: Path):
        ws_a = tmp_path / "a"
        ws_b = tmp_path / "b"
        lock_a = acquire_instance_lock(ws_a)
        lock_b = acquire_instance_lock(ws_b)
        try:
            assert lock_a.path != lock_b.path
        finally:
            lock_a.release()
            lock_b.release()

    def test_reacquire_after_release(self, tmp_path: Path):
        lock1 = acquire_instance_lock(tmp_path)
        lock1.release()
        # After release the same workspace must be lockable again.
        lock2 = acquire_instance_lock(tmp_path)
        try:
            assert isinstance(lock2, InstanceLock)
        finally:
            lock2.release()

    def test_release_is_idempotent(self, tmp_path: Path):
        lock = acquire_instance_lock(tmp_path)
        lock.release()
        # A second release must not raise.
        lock.release()
