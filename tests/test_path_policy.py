"""Tests for echo_agent.security.path_policy — denylist-based path access control."""

import os
from pathlib import Path
from unittest.mock import patch

import pytest

from echo_agent.security.path_policy import (
    check_cwd,
    check_read,
    check_write,
    resolve_path,
)

WORKSPACE = "/tmp/test-workspace"


class TestResolvePath:
    def test_absolute_path_unchanged(self):
        result = resolve_path("/home/user/file.txt", WORKSPACE)
        assert result == Path("/home/user/file.txt").resolve()

    def test_relative_path_resolved_against_workspace(self):
        result = resolve_path("subdir/file.txt", WORKSPACE)
        assert result == (Path(WORKSPACE) / "subdir" / "file.txt").resolve()

    def test_tilde_expanded(self):
        result = resolve_path("~/Documents/file.txt", WORKSPACE)
        assert str(result).startswith(str(Path.home()))


class TestCheckRead:
    def test_normal_file_allowed(self):
        assert check_read("/home/user/Desktop/notes.txt", WORKSPACE) is None

    def test_device_zero_blocked(self):
        err = check_read("/dev/zero", WORKSPACE)
        assert err is not None
        assert "device file" in err

    def test_device_random_blocked(self):
        err = check_read("/dev/random", WORKSPACE)
        assert err is not None

    def test_device_stdin_blocked(self):
        err = check_read("/dev/stdin", WORKSPACE)
        assert err is not None

    def test_proc_fd_blocked(self):
        err = check_read("/proc/self/fd/0", WORKSPACE)
        assert err is not None
        assert "stdio" in err

    def test_relative_path_allowed(self):
        assert check_read("data/file.txt", WORKSPACE) is None


class TestCheckWrite:
    def test_normal_file_allowed(self):
        assert check_write("/home/user/Desktop/notes.txt", WORKSPACE) is None

    def test_relative_path_allowed(self):
        assert check_write("output/result.json", WORKSPACE) is None

    def test_desktop_allowed(self):
        home = str(Path.home())
        assert check_write(os.path.join(home, "Desktop", "test.txt"), WORKSPACE) is None

    def test_ssh_key_blocked(self):
        home = str(Path.home())
        err = check_write(os.path.join(home, ".ssh", "id_rsa"), WORKSPACE)
        assert err is not None
        assert "denied" in err.lower()

    def test_ssh_dir_blocked(self):
        home = str(Path.home())
        err = check_write(os.path.join(home, ".ssh", "new_key"), WORKSPACE)
        assert err is not None

    def test_aws_dir_blocked(self):
        home = str(Path.home())
        err = check_write(os.path.join(home, ".aws", "credentials"), WORKSPACE)
        assert err is not None

    def test_etc_passwd_blocked(self):
        err = check_write("/etc/passwd", WORKSPACE)
        assert err is not None

    def test_etc_shadow_blocked(self):
        err = check_write("/etc/shadow", WORKSPACE)
        assert err is not None

    def test_system_path_blocked(self):
        err = check_write("/etc/nginx/nginx.conf", WORKSPACE)
        assert err is not None
        assert "system path" in err.lower()

    def test_boot_blocked(self):
        err = check_write("/boot/grub/grub.cfg", WORKSPACE)
        assert err is not None

    def test_bashrc_blocked(self):
        home = str(Path.home())
        err = check_write(os.path.join(home, ".bashrc"), WORKSPACE)
        assert err is not None

    def test_kube_blocked(self):
        home = str(Path.home())
        err = check_write(os.path.join(home, ".kube", "config"), WORKSPACE)
        assert err is not None

    def test_docker_blocked(self):
        home = str(Path.home())
        err = check_write(os.path.join(home, ".docker", "config.json"), WORKSPACE)
        assert err is not None

    def test_safe_write_root_allows_inside(self):
        assert check_write("/opt/project/file.txt", WORKSPACE, safe_write_root="/opt/project") is None

    def test_safe_write_root_blocks_outside(self):
        err = check_write("/home/user/file.txt", WORKSPACE, safe_write_root="/opt/project")
        assert err is not None
        assert "safe_write_root" in err


class TestCheckCwd:
    def test_normal_dir_allowed(self):
        assert check_cwd("/home/user/Desktop") is None

    def test_workspace_allowed(self):
        assert check_cwd(WORKSPACE) is None

    def test_proc_blocked(self):
        err = check_cwd("/proc/self")
        assert err is not None

    def test_sys_blocked(self):
        err = check_cwd("/sys/class")
        assert err is not None

    def test_tmp_allowed(self):
        assert check_cwd("/tmp") is None
