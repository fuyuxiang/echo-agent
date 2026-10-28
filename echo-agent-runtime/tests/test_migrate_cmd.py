from __future__ import annotations

import json
from pathlib import Path

from echo_agent.cli.migrate_cmd import run_migrate_command


def _seed(tmp_path: Path):
    # 造一个最小 workspace + 旧格式 user_memory.json
    mem = tmp_path / "data" / "memory"
    mem.mkdir(parents=True)
    (mem / "user_memory.json").write_text(json.dumps([{
        "id": "m1", "type": "user", "key": "user:city", "content": "上海",
        "source_session": "telegram:alice", "tags": [],
    }]), encoding="utf-8")
    # 最小配置:principal_bindings 含 telegram:alice
    cfg = tmp_path / "config.yaml"
    cfg.write_text(
        "workspace: " + str(tmp_path) + "\n"
        "memory:\n"
        "  scope_policy: session\n"
        "  owner_key: owner\n"
        "  principal_bindings:\n"
        "    - \"telegram:alice\"\n",
        encoding="utf-8",
    )
    return cfg, mem


def test_status_reports_pending(tmp_path, capsys):
    cfg, mem = _seed(tmp_path)
    rc = run_migrate_command("status", config_path=str(cfg))
    assert rc == 0
    out = capsys.readouterr().out
    assert "1" in out  # 1 条待迁移


def test_run_migrates_and_backs_up(tmp_path):
    cfg, mem = _seed(tmp_path)
    rc = run_migrate_command("run", config_path=str(cfg), yes=True)
    assert rc == 0
    data = json.loads((mem / "user_memory.json").read_text())
    assert data[0]["source_session"] == "owner"
    assert list(mem.glob("user_memory.json.migbak-*"))  # 备份已建


def test_dry_run_no_write(tmp_path):
    cfg, mem = _seed(tmp_path)
    rc = run_migrate_command("run", config_path=str(cfg), dry_run=True, yes=True)
    assert rc == 0
    data = json.loads((mem / "user_memory.json").read_text())
    assert data[0]["source_session"] == "telegram:alice"  # 未改
    assert not list(mem.glob("user_memory.json.migbak-*"))  # 未备份


def test_rollback_restores(tmp_path):
    cfg, mem = _seed(tmp_path)
    run_migrate_command("run", config_path=str(cfg), yes=True)
    rc = run_migrate_command("rollback", config_path=str(cfg), yes=True)
    assert rc == 0
    data = json.loads((mem / "user_memory.json").read_text())
    assert data[0]["source_session"] == "telegram:alice"  # 已还原


def _write_shard(mem: Path, scope: str, content: str) -> Path:
    """用 store 的真实路径规则写一个 MEMORY.<scope>.<digest>.md 渲染视图分片。"""
    from echo_agent.memory.store import MemoryStore
    store = MemoryStore(memory_dir=mem, scope_policy="session")
    store.write_long_term(scope, content)
    return store._long_term_path(scope)


def test_migrate_legacy_memory_md_adopts_owner_and_visible(tmp_path):
    # H: 旧全局 MEMORY.md(无哈希段)导入后条目归 owner_key、对 owner 可见,
    # 而非落到空 scope 造成"软失忆"。
    cfg, mem = _seed(tmp_path)
    (mem / "MEMORY.md").write_text("## user\n- **user:hobby**: 爬山", encoding="utf-8")

    rc = run_migrate_command("memory-md", config_path=str(cfg), yes=True)
    assert rc == 0

    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryType
    store = MemoryStore(memory_dir=mem, scope_policy="session")
    migrated = [e for e in store.list_all(mem_type=MemoryType.USER) if e.key == "user:hobby"]
    assert len(migrated) == 1
    assert migrated[0].source_session == "owner"
    visible = {e.key for e in store.list_all(session_key="owner")}
    assert "user:hobby" in visible


def test_migrate_memory_md_no_user_memory_no_error(tmp_path):
    # H: user_memory.json 尚不存在时 memory-md 全流程不抛异常(备份跳过)。
    mem = tmp_path / "data" / "memory"
    mem.mkdir(parents=True)
    (mem / "MEMORY.md").write_text("## user\n- **user:hobby**: 爬山", encoding="utf-8")
    cfg = tmp_path / "config.yaml"
    cfg.write_text(
        "workspace: " + str(tmp_path) + "\n"
        "memory:\n"
        "  scope_policy: session\n"
        "  owner_key: owner\n"
        "  principal_bindings: []\n",
        encoding="utf-8",
    )
    rc = run_migrate_command("memory-md", config_path=str(cfg), yes=True)
    assert rc == 0
    # 无 user_memory.json 时不建备份
    assert not list(mem.glob("user_memory.json.migbak-*"))


def test_migrate_memory_md_extracts_to_store(tmp_path):
    cfg, mem = _seed(tmp_path)
    shard = _write_shard(mem, "owner", "## user\n- **user:hobby**: 爬山\n- **user:lang**: Python")
    assert shard.exists()

    rc = run_migrate_command("memory-md", config_path=str(cfg), yes=True)
    assert rc == 0

    # 分片条目已入 store
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryType
    store = MemoryStore(memory_dir=mem, scope_policy="session")
    keys = {e.key: e.content for e in store.list_all(mem_type=MemoryType.USER)}
    assert keys.get("user:hobby") == "爬山"
    assert keys.get("user:lang") == "Python"

    # 原分片改名 .imported 备份,原名已不存在
    assert not shard.exists()
    assert shard.with_name(shard.name + ".imported").exists()


def test_migrate_memory_md_restores_real_scope_and_visible(tmp_path):
    # 分片 scope 含 : 净化后为 telegram_alice,读侧用真实 telegram:alice 比对。
    # 迁移必须靠短哈希反查还原真实 scope,否则迁移条目对该会话永久不可见。
    cfg, mem = _seed(tmp_path)  # seed 里已有 source_session=telegram:alice 的条目,作反查候选
    shard = _write_shard(mem, "telegram:alice", "## user\n- **user:hobby**: 爬山")
    assert shard.exists()
    # 文件名净化段应为 telegram_alice(不可逆),证明只能靠哈希反查
    assert ".telegram_alice." in shard.name

    rc = run_migrate_command("memory-md", config_path=str(cfg), yes=True)
    assert rc == 0

    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryType
    store = MemoryStore(memory_dir=mem, scope_policy="session")

    # 迁移条目的 source_session 必须还原为真实 scope,而非净化串
    migrated = [e for e in store.list_all(mem_type=MemoryType.USER) if e.key == "user:hobby"]
    assert len(migrated) == 1
    assert migrated[0].source_session == "telegram:alice"

    # 按真实会话键可见(走 _visible_in_session)
    visible = {e.key for e in store.list_all(session_key="telegram:alice")}
    assert "user:hobby" in visible


def test_migrate_memory_md_dry_run_no_write(tmp_path):
    cfg, mem = _seed(tmp_path)
    shard = _write_shard(mem, "owner", "## user\n- **user:hobby**: 爬山")

    rc = run_migrate_command("memory-md", config_path=str(cfg), dry_run=True, yes=True)
    assert rc == 0

    # dry-run 不改名
    assert shard.exists()
    assert not shard.with_name(shard.name + ".imported").exists()

    # dry-run 不写 store
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryType
    store = MemoryStore(memory_dir=mem, scope_policy="session")
    keys = {e.key for e in store.list_all(mem_type=MemoryType.USER)}
    assert "user:hobby" not in keys


class TestAbortedConfirmationIsNotSuccess:
    """EOF / Ctrl-C at a migration confirmation must not report success.

    The prompt helper used to sys.exit(0) on EOF, so `echo-agent migrate run`
    with a piped stdin exited 0 having migrated nothing. An abort is now treated
    exactly like a decline: nothing is written and the exit code is non-zero.
    """

    @staticmethod
    def _eof(monkeypatch):
        def _boom(_prompt):
            raise EOFError

        monkeypatch.setattr("builtins.input", _boom)

    def test_run_without_yes_aborts_nonzero(self, tmp_path, monkeypatch):
        cfg, mem = _seed(tmp_path)
        self._eof(monkeypatch)
        rc = run_migrate_command("run", config_path=str(cfg))
        assert rc == 1
        data = json.loads((mem / "user_memory.json").read_text())
        assert data[0]["source_session"] == "telegram:alice"  # 未迁移
        assert not list(mem.glob("user_memory.json.migbak-*"))  # 未备份

    def test_rollback_without_yes_aborts_nonzero(self, tmp_path, monkeypatch):
        cfg, mem = _seed(tmp_path)
        run_migrate_command("run", config_path=str(cfg), yes=True)
        self._eof(monkeypatch)
        rc = run_migrate_command("rollback", config_path=str(cfg))
        assert rc == 1
        data = json.loads((mem / "user_memory.json").read_text())
        assert data[0]["source_session"] == "owner"  # 回滚未发生

    def test_memory_md_without_yes_aborts_nonzero(self, tmp_path, monkeypatch):
        cfg, mem = _seed(tmp_path)
        shard = _write_shard(mem, "owner", "## user\n- **user:hobby**: 爬山")
        self._eof(monkeypatch)
        rc = run_migrate_command("memory-md", config_path=str(cfg))
        assert rc == 1
        assert shard.exists()
        assert not shard.with_name(shard.name + ".imported").exists()
