"""表征测试 — security/tool_policy.py + security/path_policy.py

tool_policy 覆盖目标：行 117 deny 列表命中 → False
path_policy 覆盖目标：行 218 凭证目录前缀拦截（整目录）
"""

from __future__ import annotations

import os

from echo_agent.config.schema import Config, ToolsConfig
from echo_agent.security.tool_policy import is_tool_allowed
from echo_agent.security.path_policy import check_read


# ===========================================================================
# tool_policy — deny 列表命中
# ===========================================================================

class TestIsToolAllowedDenyList:
    def _cfg_with_deny(self, denied_name: str) -> Config:
        return Config(tools=ToolsConfig(profile="full", deny=[denied_name]))

    def test_deny_list_hit_returns_false(self):
        """行 117：name in deny → False（最高优先级，无条件拒绝）。"""
        cfg = self._cfg_with_deny("exec")
        assert is_tool_allowed(cfg, "exec") is False

    def test_deny_list_hit_overrides_allow(self):
        """deny 列表比 allow 列表优先：同时在 allow 和 deny 中，应返回 False。"""
        cfg = Config(tools=ToolsConfig(allow=["exec"], deny=["exec"]))
        assert is_tool_allowed(cfg, "exec") is False

    def test_deny_list_does_not_affect_other_tools(self):
        """deny 列表只拒绝命中工具，不影响其他工具。"""
        cfg = Config(tools=ToolsConfig(profile="full", deny=["exec"]))
        # read_file 在 full profile 中应被允许
        assert is_tool_allowed(cfg, "read_file") is True

    def test_deny_list_arbitrary_name(self):
        """deny 列表可以包含任意工具名。"""
        cfg = Config(tools=ToolsConfig(profile="full", deny=["dangerous_tool_xyz"]))

        class FakeTool:
            name = "dangerous_tool_xyz"

        assert is_tool_allowed(cfg, FakeTool()) is False

    def test_empty_deny_list_allows_tool(self):
        """deny 列表为空时不拒绝工具。"""
        cfg = Config(tools=ToolsConfig(profile="full", deny=[]))
        assert is_tool_allowed(cfg, "read_file") is True


# ===========================================================================
# path_policy — 凭证目录前缀拦截（行 218）
# ===========================================================================

class TestCheckReadCredentialDirectory:
    def _home(self) -> str:
        return os.path.realpath(os.path.expanduser("~"))

    def test_ssh_directory_arbitrary_file_denied(self, tmp_path):
        """~/.ssh/my_private_key（非标准文件名）靠目录前缀被拒绝。"""
        workspace = str(tmp_path)
        ssh_dir = self._home() + "/.ssh"
        path = os.path.join(ssh_dir, "my_custom_key")
        result = check_read(path, workspace)
        assert result is not None
        assert "protected credential directory" in result or "credential" in result.lower()

    def test_gnupg_directory_file_denied(self, tmp_path):
        """~/.gnupg/ 下任意文件应被目录前缀拦截。"""
        workspace = str(tmp_path)
        gnupg_path = os.path.join(self._home(), ".gnupg", "private-keys-v1.d", "somekey.key")
        result = check_read(gnupg_path, workspace)
        assert result is not None
        assert "credential" in result.lower() or "protected" in result.lower()

    def test_aws_directory_arbitrary_file_denied(self, tmp_path):
        """~/.aws/some-profile（非 credentials 标准文件名）靠目录前缀被拒绝。"""
        workspace = str(tmp_path)
        aws_path = os.path.join(self._home(), ".aws", "some-profile")
        result = check_read(aws_path, workspace)
        assert result is not None

    def test_normal_file_in_home_allowed(self, tmp_path):
        """普通主目录文件不在凭证目录内，不应被拒绝。"""
        workspace = str(tmp_path)
        # 使用 tmp_path 中的普通文件（真实存在不影响拒绝逻辑）
        normal_path = str(tmp_path / "readme.txt")
        result = check_read(normal_path, workspace)
        assert result is None
