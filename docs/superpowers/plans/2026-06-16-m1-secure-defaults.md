# M1 安全默认档与凭证密钥 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让凭证加密开箱即用（直存合法 Fernet key + setup 自动生成落盘，删除弱 sha256-KDF），并让 gateway 公网入口在用户未显式配置时默认切到 `public_gateway` 收紧档。

**Architecture:** 两块相互独立的「默认值止血」。P0-2 新增一个职责单一的纯函数模块 `credential_key.py` 解析/生成 Fernet key，被 `CredentialManager` 与 setup 共用；P0-4 在 loader 加一个无状态窄函数 `profile_explicitly_set`，由 `run_gateway` 入口消费决定是否切档。TDD 推进：先写「修复前必失败」的回归测试，再改最小实现。不新增子系统、不改架构。

**Tech Stack:** Python 3.11，pytest（含 `pytest.mark.asyncio`），ruff，pydantic 配置（`Config`），`cryptography.fernet`，loguru。

---

## 文件结构

- `echo_agent/permissions/credential_key.py` — **新增**：`resolve_or_create_key(workspace, env_name)` 纯函数，解析或生成合法 Fernet key。
- `echo_agent/permissions/manager.py` — 修改：`CredentialManager` 增 `key_path` 入参；`_fernet()` 删 sha256-KDF 改直存 key；`_decode_secret()` 解密失败抛清晰错误。
- `echo_agent/agent/loop.py:157-161` — 修改：实例化 `CredentialManager` 时传 `key_path=workspace / ".credential_key"`。
- `echo_agent/config/loader.py` — 修改：新增 `profile_explicitly_set(config_path)` 纯函数。
- `echo_agent/app.py:355` — 修改：`run_gateway` bootstrap 后未显式配置则切 `public_gateway`。
- `echo_agent/cli/setup.py` — 修改：finalize 时调用 `resolve_or_create_key()` 并提示。
- `echo_agent/cli/i18n/zh.py` / `en.py` — 修改：新增凭证密钥提示文案 key。
- `tests/test_credential_key.py` — **新增**：P0-2 key 解析/生成/往返/旧密文不兼容回归测试。
- `tests/test_gateway_profile_default.py` — **新增**：P0-4 显式判定 + 切档行为 + 工具收紧回归测试。

---

## Task 1: P0-2 — `credential_key.py` 解析/生成 Fernet key

**Files:**
- Create: `echo_agent/permissions/credential_key.py`
- Test: `tests/test_credential_key.py`

- [ ] **Step 1: 写失败测试**

新建 `tests/test_credential_key.py`：

```python
import os
import stat
from pathlib import Path

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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pytest tests/test_credential_key.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'echo_agent.permissions.credential_key'`。

- [ ] **Step 3: 写最小实现**

新建 `echo_agent/permissions/credential_key.py`：

```python
"""Resolve or generate a valid Fernet key for credential encryption.

Single responsibility: hand back a usable Fernet key. Resolution order:
  1. ``env_name`` env var (validated as a real Fernet key)
  2. ``<workspace>/.credential_key`` file (validated)
  3. otherwise generate one and persist it with 0600 perms

This replaces the previous weak ``sha256(secret)`` KDF: we store a proper
Fernet key directly, so there is no passphrase and no derivation step.
"""

from __future__ import annotations

import os
from pathlib import Path

from cryptography.fernet import Fernet
from loguru import logger

KEY_FILENAME = ".credential_key"


def _validate(raw: bytes, *, source: str) -> bytes:
    try:
        Fernet(raw)
    except (ValueError, TypeError) as exc:
        raise ValueError(
            f"Invalid Fernet key from {source}; expected a 44-byte urlsafe-base64 "
            f"key (generate one with Fernet.generate_key())"
        ) from exc
    return raw


def resolve_or_create_key(
    workspace: Path,
    env_name: str = "ECHO_AGENT_CREDENTIAL_KEY",
) -> bytes:
    env_value = os.environ.get(env_name, "")
    if env_value:
        return _validate(env_value.encode(), source=env_name)

    key_file = Path(workspace) / KEY_FILENAME
    if key_file.exists():
        return _validate(key_file.read_bytes(), source=str(key_file))

    key = Fernet.generate_key()
    key_file.parent.mkdir(parents=True, exist_ok=True)
    key_file.write_bytes(key)
    key_file.chmod(0o600)
    logger.info("Generated credential encryption key at {}", key_file)
    return key
```

- [ ] **Step 4: 跑测试确认通过**

Run: `pytest tests/test_credential_key.py -v`
Expected: PASS（4 条全过）。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/permissions/credential_key.py tests/test_credential_key.py
git commit -m "P0-2 新增 credential_key 模块：解析或生成合法 Fernet key，替代弱 sha256 KDF"
```

---

## Task 2: P0-2 — `CredentialManager` 接入新 key 并删除弱 KDF

**Files:**
- Modify: `echo_agent/permissions/manager.py:315-364`
- Test: `tests/test_credential_key.py`（追加）

- [ ] **Step 1: 写失败测试**

追加到 `tests/test_credential_key.py` 末尾：

```python
from echo_agent.permissions.manager import CredentialManager


def test_manager_roundtrip_fernet(tmp_path, monkeypatch):
    monkeypatch.delenv(ENV, raising=False)
    store = tmp_path / "data" / "credentials.json"
    key_path = tmp_path / ".credential_key"
    mgr = CredentialManager(store_path=store, key_path=key_path)
    mgr.store("openai", "sk-secret-123")

    # 落盘后必须是 fernet 编码且不含明文
    raw = store.read_text(encoding="utf-8")
    assert '"encoding": "fernet"' in raw
    assert "sk-secret-123" not in raw

    # 新实例读回值一致
    mgr2 = CredentialManager(store_path=store, key_path=key_path)
    assert mgr2.get("openai") == "sk-secret-123"


def test_manager_rejects_undecryptable_legacy(tmp_path, monkeypatch):
    """旧 sha256-KDF 密文与新 key 不兼容时，必须清晰报错而非静默吞。"""
    monkeypatch.delenv(ENV, raising=False)
    import json
    store = tmp_path / "data" / "credentials.json"
    store.parent.mkdir(parents=True, exist_ok=True)
    # 用一把不同的 key 加密，模拟旧密文/密钥变更
    other = Fernet(Fernet.generate_key())
    store.write_text(json.dumps({
        "format": "echo-agent-credentials-v2",
        "credentials": [{
            "id": "x1", "name": "legacy", "tool_scope": "*",
            "value_hash": "", "created_at": "", "rotated_at": "",
            "encoding": "fernet", "value_enc": other.encrypt(b"old").decode(),
        }],
    }), encoding="utf-8")
    with pytest.raises(RuntimeError, match="凭证密钥"):
        CredentialManager(store_path=store, key_path=tmp_path / ".credential_key")
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pytest tests/test_credential_key.py::test_manager_roundtrip_fernet tests/test_credential_key.py::test_manager_rejects_undecryptable_legacy -v`
Expected: FAIL — `CredentialManager.__init__` 还没有 `key_path` 参数（`TypeError: unexpected keyword argument 'key_path'`）。

- [ ] **Step 3: 改 `CredentialManager.__init__`**

把 `echo_agent/permissions/manager.py:315-326` 的 `__init__`：

```python
    def __init__(
        self,
        store_path: Path,
        encryption_key_env: str = "ECHO_AGENT_CREDENTIAL_KEY",
        require_encryption: bool = False,
    ):
        self._store_path = store_path
        self._encryption_key_env = encryption_key_env
        self._require_encryption = require_encryption
        self._credentials: dict[str, Credential] = {}
        self._audit: list[dict[str, Any]] = []
        self._load()
```

改为（新增 `key_path` 入参；manager 只认一个 key 文件路径，不依赖 workspace 概念）：

```python
    def __init__(
        self,
        store_path: Path,
        encryption_key_env: str = "ECHO_AGENT_CREDENTIAL_KEY",
        require_encryption: bool = False,
        key_path: Path | None = None,
    ):
        self._store_path = store_path
        self._encryption_key_env = encryption_key_env
        self._require_encryption = require_encryption
        # Where to persist an auto-generated Fernet key when the env var is
        # unset. Defaults next to the credential store's parent (the workspace).
        self._key_path = key_path or (store_path.parent.parent / ".credential_key")
        self._credentials: dict[str, Credential] = {}
        self._audit: list[dict[str, Any]] = []
        self._load()
```

- [ ] **Step 4: 改 `_fernet()` 删除弱 KDF**

把 `echo_agent/permissions/manager.py:328-342` 的 `_fernet()`：

```python
    def _fernet(self) -> Any | None:
        secret = os.environ.get(self._encryption_key_env, "")
        if not secret:
            if self._require_encryption:
                raise RuntimeError(
                    f"Credential encryption required but {self._encryption_key_env} is not set"
                )
            return None
        try:
            import base64
            from cryptography.fernet import Fernet
        except ImportError as exc:
            raise RuntimeError("cryptography is required for encrypted credential storage") from exc
        key = base64.urlsafe_b64encode(hashlib.sha256(secret.encode()).digest())
        return Fernet(key)
```

改为（用 `resolve_or_create_key` 拿合法 key 直接用，不再过 KDF）：

```python
    def _fernet(self) -> Any | None:
        try:
            from cryptography.fernet import Fernet

            from echo_agent.permissions.credential_key import resolve_or_create_key
        except ImportError as exc:
            raise RuntimeError("cryptography is required for encrypted credential storage") from exc
        try:
            key = resolve_or_create_key(
                self._key_path.parent, env_name=self._encryption_key_env
            )
        except OSError as exc:
            # env 未设且生成落盘失败（如只读磁盘）
            if self._require_encryption:
                raise RuntimeError(
                    f"Credential encryption required but no key is available and one "
                    f"could not be created at {self._key_path}: {exc}"
                ) from exc
            return None
        return Fernet(key)
```

注意：`resolve_or_create_key` 的第一个参数是 workspace 目录，key 文件名固定为 `.credential_key`。这里传 `self._key_path.parent` 使其在该目录下生成同名文件，与 `self._key_path` 指向一致。

- [ ] **Step 5: 改 `_decode_secret()` 解密失败清晰报错**

把 `echo_agent/permissions/manager.py:353-364` 的 `_decode_secret()`：

```python
    def _decode_secret(self, item: dict[str, Any]) -> str:
        encoding = item.get("encoding", "plain")
        if encoding == "plain":
            return item.get("_value", "")
        if encoding == "fernet":
            fernet = self._fernet()
            if not fernet:
                raise RuntimeError(
                    f"Credential file is encrypted but {self._encryption_key_env} is not set"
                )
            return fernet.decrypt(item.get("value_enc", "").encode()).decode()
        raise RuntimeError(f"Unsupported credential encoding: {encoding}")
```

改为（解密失败抛清晰中文错误引导重录，不静默吞）：

```python
    def _decode_secret(self, item: dict[str, Any]) -> str:
        encoding = item.get("encoding", "plain")
        if encoding == "plain":
            return item.get("_value", "")
        if encoding == "fernet":
            fernet = self._fernet()
            if not fernet:
                raise RuntimeError(
                    f"Credential file is encrypted but {self._encryption_key_env} is not set"
                )
            from cryptography.fernet import InvalidToken
            try:
                return fernet.decrypt(item.get("value_enc", "").encode()).decode()
            except InvalidToken as exc:
                raise RuntimeError(
                    "凭证密钥已变更或损坏，无法解密现有凭证，请重新录入凭证"
                ) from exc
        raise RuntimeError(f"Unsupported credential encoding: {encoding}")
```

注意：`_load()`（`manager.py:366-383`）当前用 `except Exception: logger.warning(...)` 兜底整个加载。本设计要求解密失败为显式失败 —— 需把 `_decode_secret` 的 `RuntimeError` 透出。下一步处理。

- [ ] **Step 6: 让 `_load()` 不吞解密错误**

把 `echo_agent/permissions/manager.py:366-383` 的 `_load()` 中的兜底：

```python
        except Exception as e:
            logger.warning("Failed to load credentials: {}", e)
```

改为（解密类错误透出，其余仍兜底）：

```python
        except RuntimeError:
            raise  # 解密失败属确定性错误，必须显式暴露
        except Exception as e:
            logger.warning("Failed to load credentials: {}", e)
```

- [ ] **Step 7: 跑测试确认通过**

Run: `pytest tests/test_credential_key.py -v`
Expected: PASS（6 条全过，含新增 2 条）。

- [ ] **Step 8: 提交**

```bash
git add echo_agent/permissions/manager.py tests/test_credential_key.py
git commit -m "P0-2 CredentialManager 直存 Fernet key 删除弱 sha256 KDF，解密失败清晰报错"
```

---

## Task 3: P0-2 — loop 传 key_path

**Files:**
- Modify: `echo_agent/agent/loop.py:157-161`
- Test: 复用 Task 2（往返测试已覆盖 manager 行为）

- [ ] **Step 1: 改 `loop.py` 实例化处**

把 `echo_agent/agent/loop.py:157-161`：

```python
        self.credentials = CredentialManager(
            store_path=workspace / "data" / "credentials.json",
            encryption_key_env=config.credentials.encryption_key_env,
            require_encryption=config.credentials.require_encryption,
        )
```

改为（显式传 `key_path`，落在工作区根）：

```python
        self.credentials = CredentialManager(
            store_path=workspace / "data" / "credentials.json",
            encryption_key_env=config.credentials.encryption_key_env,
            require_encryption=config.credentials.require_encryption,
            key_path=workspace / ".credential_key",
        )
```

- [ ] **Step 2: 跑相关测试确认未破坏**

Run: `pytest tests/test_credential_key.py tests/test_cli_modules.py -v`
Expected: PASS。

- [ ] **Step 3: 提交**

```bash
git add echo_agent/agent/loop.py
git commit -m "P0-2 loop 实例化 CredentialManager 时显式传入工作区 .credential_key 路径"
```

---

## Task 4: P0-2 — setup 向导生成并提示密钥

**Files:**
- Modify: `echo_agent/cli/setup.py`（finalize 流程）
- Modify: `echo_agent/cli/i18n/zh.py`、`echo_agent/cli/i18n/en.py`
- Test: `tests/test_cli_modules.py`（追加）

- [ ] **Step 1: 写失败测试**

追加到 `tests/test_cli_modules.py` 末尾：

```python
def test_setup_ensures_credential_key(tmp_path, monkeypatch):
    """setup finalize 应在工作区生成 .credential_key（env 未设时）。"""
    monkeypatch.delenv("ECHO_AGENT_CREDENTIAL_KEY", raising=False)
    from echo_agent.cli.setup import _ensure_credential_key

    _ensure_credential_key(tmp_path)
    key_file = tmp_path / ".credential_key"
    assert key_file.exists()
    import stat as _stat
    assert _stat.S_IMODE(key_file.stat().st_mode) == 0o600
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pytest tests/test_cli_modules.py::test_setup_ensures_credential_key -v`
Expected: FAIL — `_ensure_credential_key` 不存在（`ImportError`）。

- [ ] **Step 3: 在 setup.py 新增 `_ensure_credential_key`**

在 `echo_agent/cli/setup.py` 的 `_print_summary`（`:916`）之前新增函数，并在其顶部 import 区（`:44` 附近）确保可用 `resolve_or_create_key`：

```python
def _ensure_credential_key(workspace: Path) -> None:
    """Generate the credential encryption key on first setup if absent.

    No-op when ECHO_AGENT_CREDENTIAL_KEY is set or the key file already exists.
    """
    import os

    from echo_agent.permissions.credential_key import KEY_FILENAME, resolve_or_create_key

    if os.environ.get("ECHO_AGENT_CREDENTIAL_KEY"):
        return
    key_file = Path(workspace) / KEY_FILENAME
    existed = key_file.exists()
    resolve_or_create_key(Path(workspace))
    if not existed:
        print_success(t("credentials.key_generated", path=str(key_file)))
        print_warning(t("credentials.key_warning"))
```

- [ ] **Step 4: 在 finalize 调用它**

在 `run_setup_wizard` 写配置后（`echo_agent/cli/setup.py:1091` 的 `path = save_config(...)` 之后、函数 return 之前）插入：

```python
    workspace_raw = config.get("workspace") or "~/.echo-agent"
    _ensure_credential_key(Path(str(workspace_raw)).expanduser())
```

- [ ] **Step 5: 加 i18n 文案**

i18n 已确认为**嵌套 dict**结构（`MESSAGES = {"common": {...}, "banner": {...}, ...}`），`t("credentials.key_generated")` 通过 `_resolve` 按点号路径解析嵌套。因此在 `MESSAGES` 顶层新增 `"credentials"` 分组（不要写扁平点号键）。

在 `echo_agent/cli/i18n/zh.py` 的 `MESSAGES` dict 内新增一个顶层分组：

```python
    "credentials": {
        "key_generated": "已生成凭证加密密钥并保存到 {path}（权限 0600）",
        "key_warning": "请勿删除此文件或提交到版本库，删除后已加密的凭证将无法解密",
    },
```

在 `echo_agent/cli/i18n/en.py` 的 `MESSAGES` dict 内同样新增：

```python
    "credentials": {
        "key_generated": "Generated credential encryption key at {path} (mode 0600)",
        "key_warning": "Do not delete this file or commit it to version control; deleting it makes stored credentials undecryptable",
    },
```

- [ ] **Step 6: 跑测试确认通过**

Run: `pytest tests/test_cli_modules.py::test_setup_ensures_credential_key -v`
Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add echo_agent/cli/setup.py echo_agent/cli/i18n/zh.py echo_agent/cli/i18n/en.py tests/test_cli_modules.py
git commit -m "P0-2 setup 向导首次运行自动生成凭证加密密钥并提示用户妥善保管"
```

---

## Task 5: P0-4 — `profile_explicitly_set` 纯函数

**Files:**
- Modify: `echo_agent/config/loader.py`
- Test: `tests/test_gateway_profile_default.py`

- [ ] **Step 1: 写失败测试**

新建 `tests/test_gateway_profile_default.py`：

```python
from pathlib import Path

from echo_agent.config.loader import profile_explicitly_set


def _write_yaml(tmp_path: Path, body: str) -> Path:
    f = tmp_path / "echo-agent.yaml"
    f.write_text(body, encoding="utf-8")
    return f


def test_yaml_with_profile_is_explicit(tmp_path):
    cfg = _write_yaml(tmp_path, "security:\n  profile: personal_cli\n")
    assert profile_explicitly_set(cfg) is True


def test_yaml_without_profile_is_not_explicit(tmp_path):
    cfg = _write_yaml(tmp_path, "security:\n  admin_users: []\n")
    assert profile_explicitly_set(cfg) is False


def test_no_yaml_is_not_explicit(tmp_path):
    assert profile_explicitly_set(None) is False


def test_env_profile_is_explicit(tmp_path, monkeypatch):
    cfg = _write_yaml(tmp_path, "security:\n  admin_users: []\n")
    monkeypatch.setenv("ECHO_AGENT_SECURITY__PROFILE", "public_gateway")
    assert profile_explicitly_set(cfg) is True
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pytest tests/test_gateway_profile_default.py -v`
Expected: FAIL — `ImportError: cannot import name 'profile_explicitly_set'`。

- [ ] **Step 3: 在 loader.py 新增纯函数**

在 `echo_agent/config/loader.py` 的 `load_config`（`:106`）之前新增（复用已有 `_load_yaml_file` / `_env_overrides`，只看用户来源，不含 schema/default.yaml）：

```python
def profile_explicitly_set(config_path: str | Path | None = None) -> bool:
    """Whether the user explicitly set ``security.profile``.

    Looks only at *user* sources — the resolved user YAML file and
    ``ECHO_AGENT_*`` env vars — never the packaged default.yaml or schema
    defaults. Pure: reads inputs, mutates nothing.
    """
    path = resolve_config_file(config_path)
    user_yaml = _load_yaml_file(path if path and path.exists() else None)
    if isinstance(user_yaml.get("security"), dict) and "profile" in user_yaml["security"]:
        return True
    env = _env_overrides()
    return isinstance(env.get("security"), dict) and "profile" in env["security"]
```

- [ ] **Step 4: 跑测试确认通过**

Run: `pytest tests/test_gateway_profile_default.py -v`
Expected: PASS（4 条全过）。

- [ ] **Step 5: 提交**

```bash
git add echo_agent/config/loader.py tests/test_gateway_profile_default.py
git commit -m "P0-4 loader 新增 profile_explicitly_set 纯函数，仅看用户来源判定是否显式配置"
```

---

## Task 6: P0-4 — gateway 入口默认切 public_gateway

**Files:**
- Modify: `echo_agent/app.py:341-371`（`run_gateway`）
- Test: `tests/test_gateway_profile_default.py`（追加）

- [ ] **Step 1: 写失败测试**

追加到 `tests/test_gateway_profile_default.py` 末尾（直接测「切档决策」这段可单测的逻辑；不拉起整个 runtime）：

```python
from echo_agent.app import _apply_gateway_profile_default


def test_applies_public_gateway_when_not_explicit(tmp_path):
    from echo_agent.config.schema import Config
    cfg = Config()  # 默认 security.profile == "personal_cli"
    assert cfg.security.profile == "personal_cli"
    _apply_gateway_profile_default(cfg, config_path=None)
    assert cfg.security.profile == "public_gateway"


def test_respects_explicit_profile(tmp_path):
    from echo_agent.config.schema import Config
    cfg = _write_yaml(tmp_path, "security:\n  profile: personal_cli\n")
    config = Config(security={"profile": "personal_cli"})
    _apply_gateway_profile_default(config, config_path=cfg)
    assert config.security.profile == "personal_cli"  # 显式配置不覆盖
```

- [ ] **Step 2: 跑测试确认失败**

Run: `pytest tests/test_gateway_profile_default.py::test_applies_public_gateway_when_not_explicit -v`
Expected: FAIL — `ImportError: cannot import name '_apply_gateway_profile_default'`。

- [ ] **Step 3: 在 app.py 新增切档辅助函数**

在 `echo_agent/app.py` 的 `run_gateway`（`:341`）之前新增（把决策逻辑抽成可单测的纯函数）：

```python
def _apply_gateway_profile_default(config: "Config", config_path: str | None) -> None:
    """Tighten the gateway entrypoint to ``public_gateway`` when the user did
    not explicitly choose a ``security.profile``. Explicit config is respected."""
    from echo_agent.config.loader import profile_explicitly_set

    if not profile_explicitly_set(config_path):
        config.security.profile = "public_gateway"
        logger.warning(
            "Gateway 入口未显式配置 security.profile，已默认切到 public_gateway 收紧档；"
            "如需放开请在配置中显式设置 security.profile"
        )
```

确保文件顶部已 import `Config`（用于类型注解，若仅注解可用字符串形式 `"Config"` 免循环导入）。

- [ ] **Step 4: 在 `run_gateway` 调用它**

在 `echo_agent/app.py` 的 `run_gateway` 中，`ctx.config.gateway.enabled = True`（`:356`）之后插入：

```python
    _apply_gateway_profile_default(ctx.config, config_path)
```

- [ ] **Step 5: 跑测试确认通过**

Run: `pytest tests/test_gateway_profile_default.py -v`
Expected: PASS（6 条全过）。

- [ ] **Step 6: 提交**

```bash
git add echo_agent/app.py tests/test_gateway_profile_default.py
git commit -m "P0-4 gateway 入口未显式配置 profile 时默认切 public_gateway 收紧档"
```

---

## Task 7: P0-4 — public_gateway 工具收紧锚定测试

**Files:**
- Test: `tests/test_gateway_profile_default.py`（追加）

- [ ] **Step 1: 写测试锚定切档真实生效**

追加到 `tests/test_gateway_profile_default.py` 末尾（验证 `public_gateway` 档确实 deny 高危工具、放行只读工具，防止 profile 名义切了但策略没生效）：

```python
def test_public_gateway_denies_write_allows_read():
    from echo_agent.config.schema import Config
    from echo_agent.security.tool_policy import is_tool_allowed

    cfg = Config(security={"profile": "public_gateway"})
    assert is_tool_allowed(cfg, "write_file") is False
    assert is_tool_allowed(cfg, "exec") is False
    assert is_tool_allowed(cfg, "read_file") is True
```

注意：函数名按 `echo_agent/security/tool_policy.py:107` 实际签名校准——实现前先确认对外暴露的判定函数名（文件内为 `is_tool_allowed(config, tool)` 形态；若实际名不同，按真实名改测试 import 与调用）。`read_file` 须在 `public_gateway` 的 `PROFILE_TOOLS` 允许集内，若默认 profile 工具集不含它，则改用一个确属只读且被该档允许的工具名（参照 `tool_policy.py:89` 的 `PROFILE_TOOLS`）。

- [ ] **Step 2: 跑测试**

Run: `pytest tests/test_gateway_profile_default.py::test_public_gateway_denies_write_allows_read -v`
Expected: PASS（机制已存在，本测试只锚定行为）。若 FAIL，核对工具名与 `is_tool_allowed` 签名后修正测试。

- [ ] **Step 3: 提交**

```bash
git add tests/test_gateway_profile_default.py
git commit -m "P0-4 补 public_gateway 档工具收紧锚定测试：deny 写入/执行、放行只读"
```

---

## Task 8: 全量验证

- [ ] **Step 1: 跑 lint**

Run: `ruff check .`
Expected: 无错误。若报未使用 import（如 `manager.py` 里 `hashlib` 仍被其他方法用到，确认 `value_hash` 处仍需 `hashlib`，勿误删），逐条修正。

- [ ] **Step 2: 跑全量测试**

Run: `pytest`
Expected: 全绿。若有失败，定位是否本次改动引入，修复后再次运行。

---

## 自检结论

- **Spec 覆盖**：
  - P0-2 key 模块（Task 1）、manager 直存 key + 删弱 KDF + 解密报错（Task 2）、loop 传 key_path（Task 3）、setup 生成提示 + i18n（Task 4）。
  - P0-4 显式判定纯函数（Task 5）、gateway 切档（Task 6）、工具收紧锚定（Task 7）。
  - 全量验证（Task 8）。
- **占位符**：无 TBD/TODO；所有代码步骤给出完整 before/after。两处「实现前先核对」标注（i18n 扁平/嵌套结构、`is_tool_allowed` 实际签名）属对现有代码的诚实校准点，非占位符——已给出校准方法。
- **类型一致**：`resolve_or_create_key(workspace, env_name)`、`KEY_FILENAME`、`CredentialManager(key_path=)`、`profile_explicitly_set(config_path)`、`_apply_gateway_profile_default(config, config_path)` 在定义与调用处命名一致。
- **TDD/提交**：每个 Task 先写失败测试再实现，独立提交；commit message 中文、无类型前缀、无署名（遵循项目 CLAUDE.md）。
- **非目标守护**：不做凭证迁移工具、不引入 KeyVault/轮转、不动 SSRF、不改 personal_cli CLI 体验与 daemon 档、不在 Config 上挂全局可变状态。
