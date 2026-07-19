import pytest
from echo_agent.config.schema import MemoryConfig
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.types import Contradiction, MemoryEntry


def test_auto_resolve_contradictions_defaults_false():
    cfg = MemoryConfig()
    assert cfg.auto_resolve_contradictions is False


class _FakeStore:
    def __init__(self, entries):
        self._entries = {e.id: e for e in entries}
        self.superseded = []
        self.versions = {}

    def mark_superseded(self, entry_id, superseded_by):
        self.superseded.append((entry_id, superseded_by))
        if entry_id in self._entries:
            self._entries[entry_id].superseded_by = superseded_by
        return True

    def set_version(self, entry_id, version):
        self.versions[entry_id] = version
        return True

    def _same_scope(self, a, b):
        # 镜像真实 store 的 session 策略:source_session 相等即同 scope
        # (本测试条目均无 source_session,视为同 scope,auto-resolve 照常进行)。
        return (getattr(a, "source_session", "") or "") == (getattr(b, "source_session", "") or "")


class _StubStorage:
    async def execute_sql(self, *a, **k):
        return None

    async def fetch_sql(self, *a, **k):
        # 真实 check_lightweight_sync(new, [old]) 产生 memory_id_a=new, memory_id_b=old
        return [{"memory_id_a": "new1", "memory_id_b": "old1"}]


def _make_consolidator(store):
    async def _noop_llm(**kwargs):
        raise AssertionError("LLM must not be called in auto-resolve path")
    c = MemoryConsolidator(memory_store=store, llm_call=_noop_llm)
    return c


@pytest.mark.asyncio
async def test_auto_resolve_supersedes_older_same_key(monkeypatch):
    # 显式同级来源:provenance 裁决下 legacy 不自动消解,
    # 同级(model_inferred)才走 newest-wins。
    old = MemoryEntry(id="old1", key="pref:lang", content="Python", source="model_inferred",
                      created_at="2026-06-01T00:00:00", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="new1", key="pref:lang", content="Rust", source="model_inferred",
                      created_at="2026-06-02T00:00:00", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])

    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)

    # Reuse the Contradiction Step 3 would have stored (memory_id_a=new, _b=old).
    c = Contradiction(id="c1", memory_id_a="new1", memory_id_b="old1",
                      description="Key 'pref:lang' conflict")
    resolved = await consolidator._auto_resolve_same_key(c, {"old1": old, "new1": new})
    assert resolved is True
    assert ("old1", "new1") in store.superseded


@pytest.mark.asyncio
async def test_auto_resolve_disabled_by_default():
    old = MemoryEntry(id="o", key="pref:lang", content="Python", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="n", key="pref:lang", content="Rust", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    # auto_resolve left at default False
    assert consolidator._auto_resolve_contradictions is False


@pytest.mark.asyncio
async def test_auto_resolve_gate_off_skips():
    # 与 test_auto_resolve_supersedes_older_same_key 相同的 store/detector,
    # 但不调用 set_auto_resolve_contradictions(保持默认 False)。
    # 关键:显式复现 sleep_consolidate 的 Step 3 门控语义——
    # 门控关时连 _auto_resolve_same_key 都不会被调用,因此既不消解也不 supersede。
    old = MemoryEntry(id="old1", key="pref:lang", content="Python",
                      created_at="2026-06-01T00:00:00", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="new1", key="pref:lang", content="Rust",
                      created_at="2026-06-02T00:00:00", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    # 门控保持默认 False
    assert consolidator._auto_resolve_contradictions is False

    c = Contradiction(id="c1", memory_id_a="new1", memory_id_b="old1",
                      description="Key 'pref:lang' conflict")
    # 复现 Step 3 门控:`if self._auto_resolve_contradictions and ...`
    resolved = (
        await consolidator._auto_resolve_same_key(c, {"old1": old, "new1": new})
        if consolidator._auto_resolve_contradictions
        else False
    )
    assert resolved is False
    assert store.superseded == []


@pytest.mark.asyncio
async def test_auto_resolve_skips_different_key():
    a = MemoryEntry(id="a", key="pref:lang", content="Python", updated_at="2026-06-01T00:00:00")
    b = MemoryEntry(id="b", key="pref:editor", content="vim", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([a, b])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)
    c = Contradiction(id="c1", memory_id_a="a", memory_id_b="b",
                      description="cross-key")
    resolved = await consolidator._auto_resolve_same_key(c, {"a": a, "b": b})
    assert resolved is False
    assert store.superseded == []


class _InMemoryContradictionStorage:
    """Minimal storage that honours the contradiction SQL the detector emits,
    so get_unresolved reflects real resolve() side effects."""

    def __init__(self):
        self.rows: dict[str, dict] = {}

    async def execute_sql(self, sql, params=()):
        s = " ".join(sql.split())
        if s.startswith("INSERT OR REPLACE INTO memory_contradictions"):
            cid, a, b, desc, res, res_at, created = params
            self.rows[cid] = {
                "id": cid, "memory_id_a": a, "memory_id_b": b,
                "description": desc, "resolution": res,
                "resolved_at": res_at, "created_at": created,
            }
        elif s.startswith("UPDATE memory_contradictions SET resolution"):
            res, res_at, cid = params
            if cid in self.rows:
                self.rows[cid]["resolution"] = res
                self.rows[cid]["resolved_at"] = res_at
        return None

    async def fetch_sql(self, sql, params=()):
        s = " ".join(sql.split())
        if "WHERE resolution IS NULL" in s:
            return [r for r in self.rows.values() if r["resolution"] is None]
        if "SELECT memory_id_a, memory_id_b FROM memory_contradictions WHERE id" in s:
            cid = params[0]
            return [self.rows[cid]] if cid in self.rows else []
        return []


@pytest.mark.asyncio
async def test_auto_resolve_leaves_no_ghost_unresolved():
    # 自动消解后,同 key 冲突这条记录应被原地 resolve,
    # get_unresolved 不得再返回它(无幽灵未解决行)。
    # 来源需同级非 legacy(legacy 不自动消解)。
    old = MemoryEntry(id="old1", key="pref:lang", content="Python", source="model_inferred",
                      created_at="2026-06-01T00:00:00", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="new1", key="pref:lang", content="Rust", source="model_inferred",
                      created_at="2026-06-02T00:00:00", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])
    detector = ContradictionDetector(storage=_InMemoryContradictionStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)

    # Step 3 落库的那条 c(memory_id_a=new, _b=old)。
    c = Contradiction(id="c1", memory_id_a="new1", memory_id_b="old1",
                      description="Key 'pref:lang' conflict")
    await detector.store_contradiction(c)
    assert len(await detector.get_unresolved()) == 1  # 落库后先是未解决

    resolved = await consolidator._auto_resolve_same_key(c, {"old1": old, "new1": new})
    assert resolved is True
    # 复用同一条 c 原地 resolve,无幽灵残留。
    assert await detector.get_unresolved() == []
    assert ("old1", "new1") in store.superseded


@pytest.mark.asyncio
async def test_memory_tool_lists_and_resolves_contradictions():
    from echo_agent.agent.tools.memory import MemoryTool

    class _Detector:
        def __init__(self):
            self.resolved = []
        async def get_unresolved(self, limit=10):
            return [Contradiction(id="c1", memory_id_a="a", memory_id_b="b",
                                  description="Key 'pref:lang' conflict")]
        async def resolve(self, cid, resolution, winner_id=None):
            self.resolved.append((cid, resolution, winner_id))

    from echo_agent.memory.service import MemoryService

    det = _Detector()
    tool = MemoryTool(service=MemoryService(_FakeStore([])), contradiction_detector=det)

    listed = await tool.execute({"action": "list_contradictions"})
    assert "c1" in listed.output

    done = await tool.execute({"action": "resolve_contradiction",
                               "contradiction_id": "c1", "winner_id": "a"})
    assert done.success
    assert ("c1", "a_wins", "a") in det.resolved


def test_inference_orphans_removed():
    from echo_agent.models.inference import InferenceController, InferenceConstraints
    ctrl = InferenceController()
    assert not hasattr(ctrl, "check_hallucination_markers")
    assert not hasattr(ctrl, "build_verification_prompt")
    assert not hasattr(ctrl, "layer_system_prompts")
    assert "max_output_tokens" not in InferenceConstraints.__dataclass_fields__


def test_workflow_tool_describes_orchestration_only():
    from echo_agent.agent.tools.workflow import WorkflowTool
    desc = WorkflowTool.description.lower()
    assert "orchestrat" in desc
    # 必须诚实声明引擎不自动执行 step 工具——执行者是 agent 自己,
    # 且要写清驱动协议(task start → 执行 → task complete 自动 advance)。
    assert "only orchestrates" in desc
    assert "you execute" in desc or "you are the executor" in desc
    assert "task complete" in desc


def test_sandbox_runtime_typed_checks_removed():
    from echo_agent.plugins.sandbox import PluginSandbox
    from echo_agent.plugins.manifest import PluginManifest
    sb = PluginSandbox("t", PluginManifest(name="t", permissions=["network"]), trusted=False)
    # typed runtime wrappers gone
    assert not hasattr(sb, "check_network")
    assert not hasattr(sb, "check_subprocess")
    assert not hasattr(sb, "check_filesystem_read")
    assert not hasattr(sb, "check_filesystem_write")
    # generic advisory check still works for declaration introspection
    assert sb.check_permission("network") is True
    assert sb.check_permission("subprocess") is False
    # registration-time enforcement preserved
    assert hasattr(sb, "check_tool_register")
    assert hasattr(sb, "check_hook_register")
