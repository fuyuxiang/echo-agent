"""A spill session link must never authorize writes or retention outside spill."""

import os

import pytest

from echo_agent.spill.layout import session_dir_name
from echo_agent.spill.store import SpillStore
from echo_agent.spill.sweeper import sweep


def test_spill_sweeper_does_not_follow_session_directory_links(tmp_path):
    root = tmp_path / "spill"
    root.mkdir()
    outside = tmp_path / "unrelated"
    outside.mkdir()
    victim = outside / "1234abcd-exec.txt"
    victim.write_text("user data")
    os.utime(victim, (1, 1))
    (root / session_dir_name("private")).symlink_to(outside, target_is_directory=True)
    assert sweep(root, retention_days=1, max_total_mb=0) == 0
    assert victim.read_text() == "user data"


def test_spill_writer_refuses_session_directory_links(tmp_path):
    root = tmp_path / "spill"
    root.mkdir()
    outside = tmp_path / "unrelated"
    outside.mkdir()
    (root / session_dir_name("private")).symlink_to(outside, target_is_directory=True)
    store = SpillStore(root)
    with pytest.raises(ValueError, match="symbolic link"):
        store.save("private", "exec", "private result")
    assert list(outside.iterdir()) == []


def test_spill_sweeper_does_not_follow_root_links(tmp_path):
    outside = tmp_path / "unrelated"
    session = outside / session_dir_name("private")
    session.mkdir(parents=True)
    victim = session / "1234abcd-exec.txt"
    victim.write_text("user data")
    os.utime(victim, (1, 1))
    root = tmp_path / "spill"
    root.symlink_to(outside, target_is_directory=True)
    assert sweep(root, retention_days=1, max_total_mb=0) == 0
    assert victim.exists()
