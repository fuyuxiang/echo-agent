"""★ E2 端到端闭环验收:低优先级覆盖被拒 → 写 contradiction → reflection 裁决。

这是从 R1 就锁定的 ★ 耦合点(被拒写真实 id contradiction + reflection 消费
unresolved 表)必须同期打开、不留一半的验收线。前序:
  - Task3 让 reflection._conflict_pairs 直接消费 detector.get_unresolved(不再靠
    suspected_conflict tag),并用 store.get 把两端还原成真实条目;
  - Task4 让 provenance 被拒路径把被拒内容落成真实 pending 条目(ARCHIVAL +
    needs_user_confirmation,不进召回但 store.get 可取),contradiction 行的
    memory_id_b 用真实 id(不再 blocked:<source> 占位)。

本用例真实装配 store + service + detector + reflection,串起四步验证闭环通;并额外
端到端确认两个前序审查留的语义面:
  (A) 被拒/裁决落在正确 scope,两个不同 scope 各有冲突时不跨 scope 误裁决;
  (B) 被拒落的 pending 条目确实不出现在 snapshot/retrieval/tool 召回。
"""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.eligibility import Audience
from echo_agent.memory.reflection import ReflectionEngine
from echo_agent.memory.service import MemoryService
from echo_agent.memory.service import ActorContext
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.storage.sqlite import SQLiteBackend


# ── LLM 裁决桩 ────────────────────────────────────────────────────────────────
class _FakeToolCall:
    def __init__(self, arguments: dict) -> None:
        self.arguments = arguments


class _FakeResponse:
    def __init__(self, arguments: dict) -> None:
        self.tool_calls = [_FakeToolCall(arguments)]


def _make_llm(verdict: str):
    """返回一个 async llm_call 桩:adjudicate 恒定返回给定 verdict。

    distill 阶段(save_distilled)本用例不触发——所有 key 无 ':' 前缀,
    _prefix_groups 直接跳过,故这里只需覆盖 adjudicate。"""

    async def _call(messages=None, tools=None, tool_choice=None, **kw):
        return _FakeResponse({"verdict": verdict, "explanation": "e2e stub"})

    return _call


async def _wire(tmp_path):
    """真实装配 store + service + detector + reflection(不 mock 生产逻辑)。"""
    storage = SQLiteBackend(tmp_path / "closure.db")
    await storage.initialize()
    store = MemoryStore(memory_dir=tmp_path / "mem", storage=storage)
    service = MemoryService(store)
    detector = ContradictionDetector(storage=storage, store=store, service=service)
    return storage, store, service, detector


async def _drain(store) -> None:
    """排空 _spawn_blocked_contradiction 的 fire-and-forget SQL 写任务。"""
    pending = list(store._pending_storage_tasks)
    if pending:
        await asyncio.gather(*pending, return_exceptions=True)


@pytest.mark.asyncio
async def test_low_priority_reject_then_reflection_adjudicates_closure(tmp_path):
    """四步闭环:写 → 被拒并写真实 contradiction → reflection 配对裁决 → 胜者不翻盘。"""
    storage, store, service, detector = await _wire(tmp_path)
    reflection = ReflectionEngine(
        service=service,
        llm_call=_make_llm("a_wins"),  # a=高优先级 user_stated 胜者
        contradiction_detector=detector,
    )
    try:
        # ── Step 1:user_stated 写 key=home 内容"北京"(active、高优先级)。
        winner = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="北京",
            source="user_stated", source_session="s1",
        ))

        # ── Step 2:model_inferred 同 key 覆盖"上海"→被 provenance 拒。
        store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="上海",
            source="model_inferred", source_session="s1",
        ))
        await _drain(store)

        # active 仍"北京"(被拒不改 active)。
        assert store.get(winner.id).content == "北京"
        assert not store.get(winner.id).is_superseded

        # 写了一条真实 id 的 unresolved contradiction(memory_id_b 非占位)。
        rows = await storage.fetch_sql(
            "SELECT * FROM memory_contradictions WHERE resolution IS NULL", ()
        )
        assert rows, "被拒后应写入一条 unresolved contradiction"
        row = rows[0]
        assert row["memory_id_a"] == winner.id
        assert not row["memory_id_b"].startswith("blocked:"), "memory_id_b 应为真实 id"
        landed = store.get(row["memory_id_b"])
        assert landed is not None and landed.content == "上海"

        # (B) pending 落点确实不进任何召回受众(snapshot/retrieval/tool)。
        for audience in (Audience.SNAPSHOT, Audience.RETRIEVAL, Audience.TOOL):
            visible = store._filtered_entries(
                mem_type=MemoryType.USER, session_key="s1", audience=audience,
            )
            vids = {e.id for e in visible}
            assert landed.id not in vids, f"pending 条目不应出现在 {audience} 召回"
            assert winner.id in vids, f"胜者应仍在 {audience} 召回"
        snap = store.get_snapshot(session_key="s1")
        assert "上海" not in snap and "北京" in snap

        # ── Step 3:reflection 消费 unresolved 并裁决。
        unresolved_before = await detector.get_unresolved(limit=100)
        assert any(c.id == row["id"] for c in unresolved_before), \
            "reflection 前该 contradiction 应在 get_unresolved 中"
        # 配对断言:_conflict_pairs 用 get_unresolved 把两端还原成真实条目。
        pairs = await reflection._conflict_pairs(memory_scope="s1")
        pair_ids = {frozenset((a.id, b.id)) for a, b in pairs}
        assert frozenset((winner.id, landed.id)) in pair_ids, \
            "该 contradiction 应被 _conflict_pairs 配成 (winner, landed)"

        stats = await reflection.resolve_conflicts(memory_scope="s1")
        assert stats["resolved"] == 1, f"应裁决 1 条,实际 {stats}"

        # 裁决后 resolution 非空(get_unresolved 消费掉该行)。
        after = await storage.fetch_sql(
            "SELECT resolution FROM memory_contradictions WHERE id = ?", (row["id"],)
        )
        assert after and after[0]["resolution"] == "a_wins"
        assert not await detector.get_unresolved(limit=100), \
            "裁决后不应再有 unresolved 行"

        # ── Step 4:最终 active 仍是高优先级"北京"(低优先级不因裁决翻盘)。
        assert store.get(winner.id).content == "北京"
        assert not store.get(winner.id).is_superseded, "胜者不应被 supersede"
        # 败者 pending"上海"被裁决为 superseded_by=胜者。
        assert store.get(landed.id).is_superseded
        assert store.get(landed.id).superseded_by == winner.id
    finally:
        await _drain(store)
        await storage.close()


@pytest.mark.asyncio
async def test_priority_floor_blocks_reflection_from_overturning_winner(tmp_path):
    """★ 承重:即便 LLM 裁 b_wins(低优先级"上海"胜),provenance 下限也拦下——
    高优先级"北京"不被翻盘,退化为 defer_to_user 而非 supersede。"""
    storage, store, service, detector = await _wire(tmp_path)
    reflection = ReflectionEngine(
        service=service,
        llm_call=_make_llm("b_wins"),  # b=低优先级 model_inferred,试图翻盘
        contradiction_detector=detector,
    )
    try:
        winner = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="北京",
            source="user_stated", source_session="s1",
        ))
        store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="上海",
            source="model_inferred", source_session="s1",
        ))
        await _drain(store)

        stats = await reflection.resolve_conflicts(memory_scope="s1")
        # 优先级下限拦截:不 supersede,改 defer。
        assert stats["resolved"] == 0 and stats["deferred"] == 1, f"实际 {stats}"

        # 高优先级"北京"未被翻盘,仍 active。
        assert store.get(winner.id).content == "北京"
        assert not store.get(winner.id).is_superseded, "低优先级裁决不得翻盘胜者"
        # 被 defer 后 contradiction 仍 unresolved(交用户裁),未误判为已解决。
        rows = await storage.fetch_sql(
            "SELECT * FROM memory_contradictions WHERE resolution IS NULL", ()
        )
        assert rows, "defer 路径下 contradiction 应仍 unresolved"
    finally:
        await _drain(store)
        await storage.close()


@pytest.mark.asyncio
async def test_reflection_does_not_cross_scope_adjudicate(tmp_path):
    """(A) 两个不同 scope 各有一条冲突时,裁决只在各自 scope 内落点,
    败者被本 scope 胜者 supersede,绝不出现跨 scope 误配对/误 supersede。"""
    storage, store, service, detector = await _wire(tmp_path)
    reflection = ReflectionEngine(
        service=service,
        llm_call=_make_llm("a_wins"),
        contradiction_detector=detector,
    )
    try:
        # scope s1:北京(胜) vs 上海(被拒 pending)。
        w1 = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="北京",
            source="user_stated", source_session="s1",
        ))
        store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="上海",
            source="model_inferred", source_session="s1",
        ))
        # scope s2:广州(胜) vs 深圳(被拒 pending)。
        w2 = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="广州",
            source="user_stated", source_session="s2",
        ))
        store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="深圳",
            source="model_inferred", source_session="s2",
        ))
        await _drain(store)

        rows = await storage.fetch_sql(
            "SELECT * FROM memory_contradictions WHERE resolution IS NULL", ()
        )
        assert len(rows) == 2, f"两 scope 应各写一条 contradiction,实际 {len(rows)}"
        by_a = {r["memory_id_a"]: r for r in rows}
        landed_s1 = store.get(by_a[w1.id]["memory_id_b"])
        landed_s2 = store.get(by_a[w2.id]["memory_id_b"])
        assert landed_s1.content == "上海" and landed_s1.source_session == "s1"
        assert landed_s2.content == "深圳" and landed_s2.source_session == "s2"

        # 每条 contradiction 的两端必同 scope(_spawn 落点按 source_session),
        # 不存在一条行跨 s1/s2 的配对。
        for r in rows:
            a = store.get(r["memory_id_a"])
            b = store.get(r["memory_id_b"])
            assert a.source_session == b.source_session, "contradiction 两端应同 scope"

        # 一次性跑全量裁决(memory_scope="")。
        stats = await reflection.resolve_conflicts(memory_scope="")
        assert stats["resolved"] == 2, f"两 scope 各裁一条,实际 {stats}"

        # 各 scope 胜者不动;败者仅被本 scope 胜者 supersede,无跨 scope supersede。
        assert not store.get(w1.id).is_superseded and store.get(w1.id).content == "北京"
        assert not store.get(w2.id).is_superseded and store.get(w2.id).content == "广州"
        assert store.get(landed_s1.id).superseded_by == w1.id, "s1 败者应被 s1 胜者取代"
        assert store.get(landed_s2.id).superseded_by == w2.id, "s2 败者应被 s2 胜者取代"
        assert store.get(landed_s1.id).superseded_by != w2.id
        assert store.get(landed_s2.id).superseded_by != w1.id
    finally:
        await _drain(store)
        await storage.close()


@pytest.mark.asyncio
async def test_service_reject_lands_pending_and_sql_row_in_same_await(tmp_path):
    """I: service.replace 被拒后,pending 条目与 SQL contradiction 行在同一 await 链内
    均已存在——不依赖 _spawn 的后台任务调度(不 _drain)。"""
    storage, store, service, detector = await _wire(tmp_path)
    try:
        ctx = ActorContext(actor="model", session_key="s1", memory_scope="s1")
        # 高优先级 user_stated 先落 active。
        winner = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="北京",
            source="user_stated", source_session="s1",
        ))

        # model_inferred 低优先级 replace → 被 provenance 拒。
        res = await service.replace(
            ctx, winner.id, content="上海", source="model_inferred",
        )
        assert res.ok is False and res.reason == "rejected_provenance"

        # 不 _drain:SQL contradiction 行应已在拒绝返回前 await 落盘。
        rows = await storage.fetch_sql(
            "SELECT * FROM memory_contradictions WHERE resolution IS NULL", ()
        )
        assert rows, "拒绝路径应在同一 await 链内写入 contradiction 行(无孤儿窗口)"
        row = rows[0]
        assert row["memory_id_a"] == winner.id
        landed = store.get(row["memory_id_b"])
        assert landed is not None and landed.content == "上海"
        # active 不动。
        assert store.get(winner.id).content == "北京"
        assert not store.get(winner.id).is_superseded
    finally:
        await _drain(store)
        await storage.close()
