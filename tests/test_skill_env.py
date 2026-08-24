"""Tests for the skill subprocess environment.

skill_run launched scripts with ``env={}``. An empty environment is not a smaller
one, it is a broken one: no PATH (so ``requires.bins`` was decoration and
shutil.which never found anything), no HOME (cache/config paths failed
obscurely), and no credentials — which made ``image-gen`` exit on start and led
the docs to recommend passing secrets through ``args``, where they landed in the
audit log and in ``ps`` output.

The replacement is an allowlist: infrastructure keys always, credentials only
when the skill names them in ``metadata.echo.requires.env``.
"""

from __future__ import annotations

import os
import sys

from echo_agent.skills.env import build_skill_env, declared_env_keys


SKILL_WITH_ENV = """---
name: demo
description: d
metadata:
  echo:
    requires:
      env:
        - DEMO_TOKEN
        - DEMO_REGION
---
body
"""


class TestDeclaredEnvKeys:
    def test_reads_declared_keys(self):
        assert declared_env_keys(SKILL_WITH_ENV) == ["DEMO_TOKEN", "DEMO_REGION"]

    def test_no_declaration_is_empty(self):
        assert declared_env_keys("---\nname: x\ndescription: d\n---\nbody\n") == []

    def test_accepts_scalar_form(self):
        md = "---\nname: x\ndescription: d\nmetadata:\n  echo:\n    requires:\n      env: SOLO_KEY\n---\nb\n"
        assert declared_env_keys(md) == ["SOLO_KEY"]

    def test_malformed_frontmatter_is_not_fatal(self):
        assert declared_env_keys("---\n- not-a-mapping\n---\nbody\n") == []
        assert declared_env_keys("no frontmatter at all") == []

    def test_rejects_interpreter_hijacking_keys(self):
        """PYTHONPATH & co. would let a skill choose what the next import loads."""
        md = (
            "---\nname: x\ndescription: d\nmetadata:\n  echo:\n    requires:\n"
            "      env:\n        - PYTHONPATH\n        - LD_PRELOAD\n"
            "        - DYLD_INSERT_LIBRARIES\n        - SAFE_KEY\n---\nb\n"
        )
        assert declared_env_keys(md) == ["SAFE_KEY"]

    def test_rejects_non_env_names(self):
        md = (
            "---\nname: x\ndescription: d\nmetadata:\n  echo:\n    requires:\n"
            "      env:\n        - 'not a key'\n        - 'A=B'\n        - GOOD_KEY\n---\nb\n"
        )
        assert declared_env_keys(md) == ["GOOD_KEY"]


class TestBuildSkillEnv:
    def test_path_is_present_and_leads_with_interpreter_bin(self):
        """``python3`` inside a skill script must resolve to the agent's venv."""
        env = build_skill_env("")
        assert env.get("PATH")
        first = env["PATH"].split(os.pathsep)[0]
        assert first == os.path.dirname(sys.executable)

    def test_infrastructure_keys_forwarded(self, monkeypatch):
        monkeypatch.setenv("HOME", "/home/tester")
        monkeypatch.setenv("LANG", "en_US.UTF-8")
        env = build_skill_env("")
        assert env["HOME"] == "/home/tester"
        assert env["LANG"] == "en_US.UTF-8"

    def test_proxy_and_tls_keys_forwarded(self, monkeypatch):
        """An operator behind an egress proxy has no other channel to tell a
        skill script about it, and silently bypassing it looks like a hang."""
        monkeypatch.setenv("HTTPS_PROXY", "http://proxy:3128")
        monkeypatch.setenv("SSL_CERT_FILE", "/etc/ssl/custom.pem")
        env = build_skill_env("")
        assert env["HTTPS_PROXY"] == "http://proxy:3128"
        assert env["SSL_CERT_FILE"] == "/etc/ssl/custom.pem"

    def test_declared_credential_is_forwarded(self, monkeypatch):
        monkeypatch.setenv("DEMO_TOKEN", "s3cr3t")
        env = build_skill_env(SKILL_WITH_ENV)
        assert env["DEMO_TOKEN"] == "s3cr3t"

    def test_undeclared_credential_is_withheld(self, monkeypatch):
        """The whole point of the allowlist: reading a SKILL.md tells you exactly
        which secrets it can see."""
        monkeypatch.setenv("UNRELATED_API_KEY", "nope")
        env = build_skill_env(SKILL_WITH_ENV)
        assert "UNRELATED_API_KEY" not in env

    def test_declared_but_unset_is_simply_absent(self, monkeypatch):
        monkeypatch.delenv("DEMO_REGION", raising=False)
        env = build_skill_env(SKILL_WITH_ENV)
        assert "DEMO_REGION" not in env

    def test_environment_is_not_wholesale_inherited(self, monkeypatch):
        monkeypatch.setenv("SOME_RANDOM_HOST_VAR", "leak-me")
        env = build_skill_env("")
        assert "SOME_RANDOM_HOST_VAR" not in env

    def test_lazy_install_switch_propagates(self, monkeypatch):
        """A script reaching back into the agent's machinery must see the same
        policy the parent runs under."""
        monkeypatch.setenv("ECHO_AGENT_DISABLE_LAZY_INSTALLS", "1")
        env = build_skill_env("")
        assert env["ECHO_AGENT_DISABLE_LAZY_INSTALLS"] == "1"

    def test_skill_cannot_hijack_path_via_declaration(self, monkeypatch):
        """PATH is set by us; a skill asking for it must not override that."""
        md = (
            "---\nname: x\ndescription: d\nmetadata:\n  echo:\n    requires:\n"
            "      env:\n        - PATH\n---\nb\n"
        )
        monkeypatch.setenv("PATH", "/attacker/bin")
        env = build_skill_env(md)
        assert env["PATH"].split(os.pathsep)[0] == os.path.dirname(sys.executable)

    def test_base_override_for_testing(self):
        env = build_skill_env("", base={"HOME": "/x", "IGNORED": "y"})
        assert env["HOME"] == "/x"
        assert "IGNORED" not in env
