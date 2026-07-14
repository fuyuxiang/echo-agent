"""目标达成验证(performance goal verification)——不是单元测试，而是回答
"回复路径真的瘦身了吗 / 流式真的让首 token 早到吗"。

分三组,对应架构师对"快回复环"改造的因果主张:
- A 结构验证:命中预取缓存的回合,首 token 前不再发生昂贵的检索/同步扫描调用。
- B 微基准:把昂贵检索注入人造延迟,证明改后的快路径不再为它阻塞(相对量化)。
- C 流式时序:真流式让首个 delta 在整段回复完成之前就到达。

注意诚实边界:这些验证的是"我们移走的阻塞确实被移走了、流式确实边吐边到"
(结构层面的目标达成)。用户感知的真实首 token 毫秒数需要真实模型 API + 网络,
不在本套验证范围内——见文件末尾 test_real_latency_requires_live_api 的说明。
"""
import asyncio
import time
from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.bus.events import InboundEvent
from echo_agent.memory.prefetch import RetrievalCacheEntry, query_tokens
from echo_agent.models.provider import LLMProvider, LLMResponse, _invoke_stream_callback
from echo_agent.session.manager import Session


def _build_stage(*, cache, on_miss, knowledge=None, episodic=None,
                 retrieve_spy=None):
    """构造一个带探针的 ContextStage。retrieve_spy / knowledge.search 用来
    数"首 token 前昂贵检索被调了几次"。"""
    config = MagicMock()
    config.session.max_history_messages = 100
    config.memory.enabled = True
    config.gateway.emit_progress_events = True
    config.gateway.progress_debug = False
    config.knowledge.max_results = 5

    sessions = AsyncMock()
    sessions.save = AsyncMock()
    memory = MagicMock()
    memory.get_snapshot = MagicMock(return_value="")
    memory.search_scored = MagicMock(return_value=[])

    compressor = MagicMock()
    compressor.should_compress = MagicMock(return_value=False)

    context_builder = MagicMock()
    context_builder.build_system_prompt = MagicMock(return_value="sys")
    context_builder.build_messages = MagicMock(
        side_effect=lambda **kw: [{"role": "user", "content": kw.get("current_message", "")}]
    )

    inference = MagicMock()
    inference.filter_tools = MagicMock(return_value=[])

    hybrid_retriever = MagicMock()
    hybrid_retriever.retrieve = AsyncMock(
        side_effect=retrieve_spy or (lambda *a, **k: [])
    )

    stage = ContextStage(
        config=config, sessions=sessions, memory=memory, compressor=compressor,
        context_builder=context_builder, skill_store=None, knowledge=knowledge,
        hybrid_retriever=hybrid_retriever, planner=None, inference=inference,
        working_memories=OrderedDict(), memory_snapshots=OrderedDict(),
        snapshot_enabled=False, tool_definitions_fn=lambda: [], episodic=episodic,
        retrieval_cache_get=lambda sk: cache.get(sk),
        retrieval_on_miss=on_miss, cache_ttl=60.0, cache_jaccard_min=0.3,
    )
    return stage, hybrid_retriever


async def _build(stage, *, session_key, text, sender_id="u1"):
    event = InboundEvent.text_message(
        channel="cli", sender_id=sender_id, chat_id="c1", text=text
    )
    event.session_key_override = session_key
    return await stage.build(
        event, Session(key=session_key),
        publish_response=False, trace_id="t1", stream_publisher=None, intro_text="",
    )


def _fresh_entry(text, *, scored=None, episodes=None, knowledge_context=None,
                 knowledge_user_id=None):
    return RetrievalCacheEntry(
        query_text=text, query_tokens=query_tokens(text),
        scored=scored or [], created_at=time.time(),
        episodes=episodes, knowledge_context=knowledge_context,
        knowledge_user_id=knowledge_user_id,
    )


# ====================================================================
# A. 结构验证:命中缓存的回合,首 token 前零昂贵检索调用
# ====================================================================

@pytest.mark.asyncio
async def test_A_cache_hit_runs_zero_inline_retrieval():
    """主记忆 + episodic + knowledge 全部命中新鲜缓存时,ContextStage.build
    在首 token 前不调用任何昂贵检索(retrieve / knowledge.search)。"""
    knowledge = MagicMock()
    knowledge.search_async = AsyncMock(side_effect=AssertionError("不应在快路径同步扫描知识库"))
    knowledge.format_results = MagicMock(return_value="kb")

    episodic = MagicMock()
    episodic.search_episodes = AsyncMock(side_effect=AssertionError("不应在快路径查 episodic"))

    entry = _fresh_entry(
        "deploy gateway",
        scored=[(MagicMock(key="k", content="cached mem"), 0.9)],
        episodes=[MagicMock(summary="cached ep")],
        knowledge_context="cached kb",
        knowledge_user_id="u1",
    )
    stage, hybrid = _build_stage(
        cache={"s": entry}, on_miss="sync", knowledge=knowledge, episodic=episodic,
    )
    await _build(stage, session_key="s", text="gateway deploy steps", sender_id="u1")

    # 核心断言:三路昂贵检索在首 token 前的调用次数 = 0
    assert hybrid.retrieve.await_count == 0, "主记忆检索不应在快路径内联执行"
    assert knowledge.search_async.call_count == 0, "knowledge 同步扫描不应在快路径执行"
    assert episodic.search_episodes.await_count == 0, "episodic 不应在快路径执行"


@pytest.mark.asyncio
async def test_A_cli_degrade_miss_skips_retrieval_entirely():
    """CLI 默认(degrade)下缓存未命中:检索变为有界同步(时间预算内),
    knowledge/episodic 的独立内联扫描仍被跳过;timeout=0 保留完全跳过的
    旧快路径。"""
    knowledge = MagicMock()
    knowledge.search_async = AsyncMock(side_effect=AssertionError("degrade 不应内联扫描"))
    episodic = MagicMock()
    episodic.search_episodes = AsyncMock(side_effect=AssertionError("degrade 不应查 episodic"))

    # 默认预算:miss 走有界检索(单次 retrieve 调用,预算封顶不阻塞)
    stage, hybrid = _build_stage(
        cache={}, on_miss="degrade", knowledge=knowledge, episodic=episodic,
    )
    await _build(stage, session_key="new", text="anything", sender_id="u1")
    assert hybrid.retrieve.await_count == 1
    assert knowledge.search_async.call_count == 0
    assert episodic.search_episodes.await_count == 0

    # timeout=0:旧的完全跳过快路径仍可配置
    stage0, hybrid0 = _build_stage(
        cache={}, on_miss="degrade", knowledge=knowledge, episodic=episodic,
    )
    stage0._retrieval_miss_timeout = 0.0
    await _build(stage0, session_key="new", text="anything", sender_id="u1")
    assert hybrid0.retrieve.await_count == 0
    assert knowledge.search_async.call_count == 0
    assert episodic.search_episodes.await_count == 0


# ====================================================================
# B. 微基准:给昂贵检索注入人造延迟,证明命中缓存的快路径不再为它阻塞
#    (相对量化——人造 200ms 代表同步全量扫描,不是真实 TF-IDF 耗时)
# ====================================================================

@pytest.mark.asyncio
async def test_B_cache_hit_avoids_injected_blocking_cost():
    BLOCK = 0.2  # 200ms 人造阻塞,代表同步全量扫描

    def _slow_search(*a, **k):
        time.sleep(BLOCK)  # 同步阻塞,模拟 knowledge TF-IDF 全量扫描
        return [object()]

    # 旧行为基线:未命中 + sync,knowledge 内联(虽走 executor,墙钟仍要等)
    knowledge = MagicMock()
    knowledge.search_async = AsyncMock(side_effect=_slow_search)
    knowledge.format_results = MagicMock(return_value="kb")
    stage_miss, _ = _build_stage(cache={}, on_miss="sync", knowledge=knowledge)
    t0 = time.perf_counter()
    await _build(stage_miss, session_key="s", text="q", sender_id="u1")
    miss_cost = time.perf_counter() - t0

    # 改后:命中缓存,knowledge 不执行
    knowledge2 = MagicMock()
    knowledge2.search_async = AsyncMock(side_effect=_slow_search)
    knowledge2.format_results = MagicMock(return_value="kb")
    entry = _fresh_entry("q", knowledge_context="cached kb", knowledge_user_id="u1")
    stage_hit, _ = _build_stage(cache={"s": entry}, on_miss="sync", knowledge=knowledge2)
    t1 = time.perf_counter()
    await _build(stage_hit, session_key="s", text="q", sender_id="u1")
    hit_cost = time.perf_counter() - t1

    # 核心断言用"是否触发昂贵检索"来判定,而非绝对墙钟:命中路径根本不该
    # 调用 knowledge.search,这是确定性的,不受 CI 负载下的事件循环调度抖动影响。
    knowledge2.search_async.assert_not_called()
    knowledge.search_async.assert_called()  # 基线确实触发了注入的阻塞检索

    # 命中路径省掉了那 200ms 阻塞:相对基线应快出至少一个 BLOCK 的量级。
    # 不再用 hit_cost < BLOCK/2 这种绝对上限——负载下纯 await 调度延迟即可
    # 超过 100ms,与"是否跳过检索"无关,会造成 flaky。
    assert miss_cost >= BLOCK, f"基线 {miss_cost:.3f}s 未体现注入的阻塞(基准无效)"
    assert miss_cost - hit_cost >= BLOCK / 2, (
        f"命中 {hit_cost:.3f}s 相对基线 {miss_cost:.3f}s 提速不足,疑未跳过阻塞检索"
    )


# ====================================================================
# C. 流式时序:真流式让首个 delta 在整段回复完成之前到达
# ====================================================================

class _TimedStreamProvider(LLMProvider):
    """每个 token 间隔 STEP 秒吐出,共 N 个。真流式下首 delta 应在 ~STEP 到达,
    而不是等 N*STEP 全部完成。"""
    STEP = 0.05
    N = 8

    def __init__(self):
        super().__init__(api_key="k", api_base="b")

    def get_default_model(self):
        return "timed"

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        await asyncio.sleep(self.STEP * self.N)
        return LLMResponse(content="x" * self.N, finish_reason="stop")

    async def chat_stream(self, messages, tools=None, model=None,
                          tool_choice=None, on_delta=None, **kwargs):
        for _ in range(self.N):
            await asyncio.sleep(self.STEP)
            await _invoke_stream_callback(on_delta, "x")
        return LLMResponse(content="x" * self.N, finish_reason="stop")


@pytest.mark.asyncio
async def test_C_first_delta_arrives_before_full_completion():
    p = _TimedStreamProvider()
    start = time.perf_counter()
    first_delta_at = {"t": None}

    def _on_delta(_):
        if first_delta_at["t"] is None:
            first_delta_at["t"] = time.perf_counter() - start

    resp = await p.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hi"}], on_delta=_on_delta
    )
    total = time.perf_counter() - start

    # 首 delta 应在整段完成前明显到达——这正是"真流式让首 token 早到"的证据
    assert first_delta_at["t"] is not None, "从未收到流式 delta"
    assert first_delta_at["t"] < total / 2, (
        f"首 delta 在 {first_delta_at['t']:.3f}s 才到,总耗时 {total:.3f}s——不像真流式"
    )
    assert resp.content == "x" * _TimedStreamProvider.N


# ====================================================================
# D. 真实首 token 延迟——本环境无法验证,诚实标注
# ====================================================================

@pytest.mark.skip(
    reason="真实首 token 毫秒数需要真模型 API + 网络;本套验证只覆盖"
    "结构(A)、相对阻塞成本(B)、流式时序(C)。绝对延迟请在有 key 的"
    "环境跑对比脚本(改造前 commit vs 改造后),不在离线测试范围。"
)
def test_real_latency_requires_live_api():
    pass
