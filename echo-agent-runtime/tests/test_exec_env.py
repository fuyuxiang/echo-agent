"""Tests for exec-subprocess environment construction.

The exec tool used to hand every command the whole parent environment,
including every API key the agent itself holds. A command only needs the
credentials the operator explicitly handed it via the tool's ``credentials``
argument — anything else is a leak waiting to happen (``ps`` output, a
subcommand that echoes ``env``, a dependency that phones home).

Only known process infrastructure is inherited. Secrets and task-specific
configuration require an explicit per-call grant.
"""

from __future__ import annotations

import pytest

from echo_agent.security.exec_env import build_exec_env, selected_ambient_env


def _base() -> dict[str, str]:
    return {
        "PATH": "/usr/bin:/bin",
        "HOME": "/home/tester",
        "LANG": "en_US.UTF-8",
        "OPENAI_API_KEY": "sk-secret",
        "GITHUB_TOKEN": "ghp-secret",
        "DB_PASSWORD": "hunter2",
        "AWS_SECRET_ACCESS_KEY": "aws-secret",
        "DATABASE_URL": "postgres://user:pass@db/app",
        "PIP_INDEX_URL": "https://user:pass@packages.example/simple",
        "WORKSPACE": "/srv/app",
        "CUSTOM_CONFIG": "/etc/app.ini",
    }


class TestSecretStripping:
    def test_secret_named_vars_are_dropped(self):
        env = build_exec_env(_base())
        assert "OPENAI_API_KEY" not in env
        assert "GITHUB_TOKEN" not in env
        assert "DB_PASSWORD" not in env
        assert "AWS_SECRET_ACCESS_KEY" not in env
        assert "DATABASE_URL" not in env
        assert "PIP_INDEX_URL" not in env

    def test_case_insensitive(self):
        env = build_exec_env({"my_token": "x", "API_Key": "y"})
        assert "my_token" not in env
        assert "API_Key" not in env

    def test_non_secret_vars_are_kept(self):
        env = build_exec_env(_base())
        assert env["PATH"] == "/usr/bin:/bin"
        assert env["HOME"] == "/home/tester"
        assert env["LANG"] == "en_US.UTF-8"
        assert "WORKSPACE" not in env
        assert "CUSTOM_CONFIG" not in env

    def test_arbitrary_names_are_not_ambient_grants(self):
        env = build_exec_env({"KEYBOARD_LAYOUT": "us", "TOKENIZER": "bpe"})
        assert env == {}


class TestInjectionKeyStripping:
    def test_dynamic_loader_injection_keys_are_dropped(self):
        env = build_exec_env({
            "LD_PRELOAD": "/tmp/evil.so",
            "DYLD_INSERT_LIBRARIES": "/tmp/evil.dylib",
            "BASH_ENV": "/tmp/evil.sh",
        })
        assert "LD_PRELOAD" not in env
        assert "DYLD_INSERT_LIBRARIES" not in env
        assert "BASH_ENV" not in env

    def test_pythonpath_requires_explicit_grant(self):
        env = build_exec_env({"PYTHONPATH": "/srv/app/src"})
        assert "PYTHONPATH" not in env
        explicit = build_exec_env({}, extra={"PYTHONPATH": "/srv/app/src"})
        assert explicit["PYTHONPATH"] == "/srv/app/src"

    def test_authenticated_proxy_is_not_inherited(self):
        env = build_exec_env({
            "HTTP_PROXY": "http://user:pass@proxy.example:8080",
            "HTTPS_PROXY": "proxy.example:8080",
        })
        assert "HTTP_PROXY" not in env
        assert env["HTTPS_PROXY"] == "proxy.example:8080"


class TestExplicitWins:
    def test_explicit_credentials_survive_stripping(self):
        env = build_exec_env(_base(), credentials={"MY_TOKEN": "s3cr3t"})
        assert env["MY_TOKEN"] == "s3cr3t"

    def test_explicit_extra_overrides_stripping(self):
        env = build_exec_env(_base(), extra={"OPENAI_API_KEY": "explicit-key"})
        assert env["OPENAI_API_KEY"] == "explicit-key"

    def test_per_call_credential_overrides_allowlisted_ambient_value(self):
        env = build_exec_env(
            _base(), extra={"OPENAI_API_KEY": "stale-ambient"},
            credentials={"OPENAI_API_KEY": "selected-for-this-call"},
        )
        assert env["OPENAI_API_KEY"] == "selected-for-this-call"

    def test_source_dict_is_not_mutated(self):
        base = _base()
        build_exec_env(base)
        assert "OPENAI_API_KEY" in base

    def test_only_named_ambient_values_are_selected(self):
        selected = selected_ambient_env(["DATABASE_URL", "PYTHONPATH"], {
            "DATABASE_URL": "postgres://user:pass@db/app",
            "OPENAI_API_KEY": "private",
        })
        assert selected == {"DATABASE_URL": "postgres://user:pass@db/app"}

    def test_invalid_allowlist_name_is_rejected(self):
        from echo_agent.config.schema import ExecToolConfig

        with pytest.raises(ValueError, match="Invalid environment variable name"):
            ExecToolConfig(env_allowlist=["BAD=NAME"])
        config = ExecToolConfig(env_allowlist=["DATABASE_URL", "DATABASE_URL"])
        assert config.env_allowlist == ["DATABASE_URL"]

    def test_allowlist_loads_from_yaml_alias_and_environment_override(self, monkeypatch):
        from echo_agent.config.loader import _env_overrides
        from echo_agent.config.schema import Config

        yaml_style = Config.model_validate({
            "tools": {"exec": {"envAllowlist": ["DATABASE_URL"]}},
        })
        assert yaml_style.tools.exec.env_allowlist == ["DATABASE_URL"]

        monkeypatch.setenv("ECHO_AGENT_TOOLS__EXEC__ENV_ALLOWLIST", '["PYTHONPATH"]')
        from_env = Config.model_validate(_env_overrides())
        assert from_env.tools.exec.env_allowlist == ["PYTHONPATH"]


class TestLocalExecutorWiring:
    """Pin the executor and direct tool paths to the safe environment."""

    @pytest.mark.asyncio
    async def test_ambient_secret_does_not_reach_subprocess(self, tmp_path, monkeypatch):
        from echo_agent.agent.executors.base import ExecRequest, LocalExecutor

        monkeypatch.setenv("DATABASE_URL", "postgres://user:pass@db/app")
        ex = LocalExecutor(workspace=str(tmp_path))
        await ex.setup()
        resp = await ex.execute(ExecRequest(command="echo ${DATABASE_URL:-unset}"))
        assert resp.stdout.strip() == "unset"
        await ex.teardown()

    @pytest.mark.asyncio
    async def test_explicit_credential_does_reach_subprocess(self, tmp_path):
        from echo_agent.agent.executors.base import ExecRequest, LocalExecutor

        ex = LocalExecutor(workspace=str(tmp_path))
        await ex.setup()
        resp = await ex.execute(ExecRequest(
            command="echo $MY_TOKEN",
            credentials={"MY_TOKEN": "s3cr3t"},
        ))
        assert "s3cr3t" in resp.stdout
        await ex.teardown()

    @pytest.mark.asyncio
    async def test_direct_shell_and_code_paths_do_not_inherit_url_secret(self, tmp_path, monkeypatch):
        from echo_agent.agent.tools.code_exec import CodeExecTool
        from echo_agent.agent.tools.shell import ShellTool

        monkeypatch.setenv("DATABASE_URL", "postgres://user:pass@db/app")
        shell = ShellTool(workspace=str(tmp_path))
        shell_result = await shell.execute({"command": "echo ${DATABASE_URL:-unset}"})
        assert shell_result.success
        assert shell_result.output.strip() == "unset"

        code = CodeExecTool(workspace=str(tmp_path))
        code_result = await code.execute({
            "language": "python",
            "code": "import os; print(os.environ.get('DATABASE_URL', 'unset'))",
        })
        assert code_result.success
        assert code_result.output.strip() == "unset"

    @pytest.mark.asyncio
    async def test_allowlist_reaches_all_three_tools(self, tmp_path, monkeypatch):
        import asyncio

        from echo_agent.agent.tools.code_exec import CodeExecTool
        from echo_agent.agent.tools.process import ProcessTool
        from echo_agent.agent.tools.shell import ShellTool

        monkeypatch.setenv("DATABASE_URL", "project-db")
        monkeypatch.setenv("OPENAI_API_KEY", "must-stay-private")

        shell = ShellTool(str(tmp_path), env_allowlist=["DATABASE_URL"])
        shell_result = await shell.execute({
            "command": "echo ${DATABASE_URL:-unset} ${OPENAI_API_KEY:-unset}",
        })
        assert shell_result.success
        assert shell_result.output.strip() == "project-db unset"

        code = CodeExecTool(str(tmp_path), env_allowlist=["DATABASE_URL"])
        code_result = await code.execute({
            "language": "python",
            "code": "import os; print(os.environ.get('DATABASE_URL'), os.environ.get('OPENAI_API_KEY', 'unset'))",
        })
        assert code_result.success
        assert code_result.output.strip() == "project-db unset"

        process = ProcessTool(str(tmp_path), env_allowlist=["DATABASE_URL"])
        try:
            started = await process.execute({
                "action": "start",
                "command": "echo ${DATABASE_URL:-unset} ${OPENAI_API_KEY:-unset}",
            })
            assert started.success
            pid = started.metadata["process_id"]
            await asyncio.wait_for(process._processes[pid]["collector"], timeout=5)
            polled = await process.execute({"action": "poll", "process_id": pid})
            assert "project-db unset" in polled.output
        finally:
            await process.aclose()

    @pytest.mark.asyncio
    async def test_process_default_does_not_inherit_url_secret(self, tmp_path, monkeypatch):
        import asyncio

        from echo_agent.agent.tools.process import ProcessTool

        monkeypatch.setenv("DATABASE_URL", "postgres://user:pass@db/app")
        process = ProcessTool(str(tmp_path))
        try:
            started = await process.execute({
                "action": "start", "command": "echo ${DATABASE_URL:-unset}",
            })
            assert started.success
            pid = started.metadata["process_id"]
            await asyncio.wait_for(process._processes[pid]["collector"], timeout=5)
            polled = await process.execute({"action": "poll", "process_id": pid})
            assert "unset" in polled.output
        finally:
            await process.aclose()
