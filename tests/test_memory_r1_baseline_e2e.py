# tests/test_memory_r1_baseline_e2e.py
import json, pytest
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.service import MemoryService, ActorContext
from echo_agent.memory.types import MemoryEntry, MemoryType

def _svc(tmp_path):
    calls = []
    async def _inval(scope, g): calls.append((scope, g))
    async def _flush(): return 0
    audit = tmp_path / "logs" / "memory_audit.jsonl"
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    svc = MemoryService(store, invalidate_fn=_inval, flush_fn=_flush,
                        audit_path=audit, allow_env_writes=False)
    return svc, store, calls, audit

@pytest.mark.asyncio
async def test_write_triggers_invalidation(tmp_path):
    svc, _, calls, _ = _svc(tmp_path)
    await svc.add(ActorContext(actor="model", session_key="s", memory_scope="x"),
                  type=MemoryType.USER, key="k", content="v", source="user_stated")
    assert ("x", False) in calls  # USER 写触发本 scope 失效

@pytest.mark.asyncio
async def test_env_denied_by_default(tmp_path):
    svc, _, _, _ = _svc(tmp_path)
    r = await svc.add(ActorContext(actor="model", session_key="s", memory_scope="x"),
                      type=MemoryType.ENVIRONMENT, key="os", content="Linux", source="model_inferred")
    assert r.ok is False and r.reason == "rejected_env"

@pytest.mark.asyncio
async def test_low_priority_replace_rejected(tmp_path):
    svc, store, _, _ = _svc(tmp_path)
    e = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                              source="user_stated", source_session="x"))
    r = await svc.replace(ActorContext(actor="model", session_key="s", memory_scope="x"),
                          e.id, content="北京", source="model_inferred")
    assert r.ok is False and r.reason == "rejected_provenance"

@pytest.mark.asyncio
async def test_audit_jsonl_written(tmp_path):
    svc, _, _, audit = _svc(tmp_path)
    await svc.add(ActorContext(actor="model", session_key="s", memory_scope="x"),
                  type=MemoryType.USER, key="k", content="v", source="user_stated")
    lines = audit.read_text(encoding="utf-8").strip().splitlines()
    rec = json.loads(lines[-1])
    assert rec["actor"] == "model" and rec["op"] == "add" and rec["ok"] is True
