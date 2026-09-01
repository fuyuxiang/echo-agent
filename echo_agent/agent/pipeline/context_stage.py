"""Context stage — assembles system prompt, memory, retrieval, and messages for LLM."""

from __future__ import annotations

import copy
import re
import time
from collections import OrderedDict
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.agent.context import (
    ContextBuilder,
    build_capabilities_context,
    build_memory_context,
    build_skills_context,
)
from echo_agent.agent.pipeline.response_stage import _is_ephemeral_session
from echo_agent.agent.pipeline.types import PipelineContext
from echo_agent.bus.events import InboundEvent
from echo_agent.memory.eligibility import Audience
from echo_agent.session.manager import Session

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from echo_agent.agent.compression import ConversationCompressor
    from echo_agent.agent.cognitive_emitter import CognitiveEmitter
    from echo_agent.agent.planning.planner import AgentPlanner
    from echo_agent.config.schema import Config
    from echo_agent.knowledge.index import KnowledgeIndex
    from echo_agent.memory.retriever import HybridRetriever
    from echo_agent.memory.store import MemoryStore
    from echo_agent.models.inference import InferenceController
    from echo_agent.session.manager import SessionManager
    from echo_agent.skills.store import SkillStore


def filter_recall_by_snapshot(scored, snapshot_ids):
    """Drop scored memory entries whose id already entered the frozen snapshot.
    Defensive: entries without an id, or a falsy snapshot_ids, are kept as-is."""
    if not snapshot_ids:
        return scored
    return [
        (r, s) for (r, s) in scored
        if getattr(r, "id", None) not in snapshot_ids
    ]


_REPLY_SNIPPET_MAX = 500  # 被引用原文注入上限，过长截断，避免撑爆上下文

# 「继续上一个任务」的意图标记。刻意保守:只有当用户消息基本只包含继续指令
# (≤12 字符)时才续跑旧计划——一条新的完整问题即使包含"继续"一词,也应视为
# 新任务走 create_plan。宁可漏续(用户可再说一次"继续"),不可错续。
_RESUME_MARKERS = ("继续", "接着做", "接着来", "继续做", "continue", "resume", "go on")
_ARTIFACT_CONTINUATION_VERSION = 1
_ARTIFACT_CONTINUATION_TTL_SECONDS = 30 * 60
_ARTIFACT_NO_FILE_MARKERS = (
    "不要文件", "不用文件", "无需文件", "无须文件", "不要附件", "不用附件",
    "不要保存", "不用保存", "chat only", "no file", "no attachment",
    "without a file", "do not save", "don't save",
)
_ARTIFACT_CHAT_OUTPUT_RE = re.compile(
    r"(?:直接在(?:聊天|对话)|直接(?:回答|回复)|聊天里回答|"
    r"(?:answer|respond|reply)\s+(?:directly\s+)?in\s+(?:the\s+)?chat)",
    re.IGNORECASE,
)
_ARTIFACT_CHAT_NEGATION_RE = re.compile(
    r"(?:不要|别|不需|无需|(?:^|\s)do\s+not|(?:^|\s)don't|(?:^|\s)not)\s*$",
    re.IGNORECASE,
)
_ARTIFACT_HOW_TO_RE = re.compile(
    r"(?:如何|怎么|怎样|请问.{0,8}怎么).{0,20}"
    r"(?:写|撰写|生成|创建|制作|导出|保存).{0,20}"
    r"(?:报告|文档|文件|稿件)|"
    r"(?:^|\s)(?:how\s+to|how\s+(?:do|can|should)\s+i)(?:\s|$).{0,40}"
    r"(?:write|create|generate|produce|export|save).{0,30}"
    r"(?:report|document|file|manuscript)",
    re.IGNORECASE,
)
_ARTIFACT_DOC_TRANSFORM_RE = re.compile(
    # A document noun can describe the INPUT. These imperative-looking prompts
    # ask for a short chat transformation of that input, not a new file.
    r"^\s*(?:请(?:你)?|请帮我|帮我|麻烦(?:你)?)?\s*(?:写|撰写)"
    r".{0,80}(?:三|五|几|一|两|\d+)\s*(?:句话|句|段话|段|个要点)|"
    r"^\s*(?:请(?:你)?|请帮我|帮我|麻烦(?:你)?)?\s*(?:写|撰写)"
    r".{0,80}(?:总结|概括|摘要|评价|点评|回复|回答).{0,30}"
    r"(?:这份|该|下面的|以下的|上述)?(?:报告|文档|稿件)|"
    r"^\s*(?:please\s+)?(?:help\s+me\s+)?write\s+.{0,100}"
    r"(?:sentences?|paragraphs?|bullets?|summary|response|review|comments?)"
    r".{0,50}(?:this|the|following|attached)\s+(?:report|document|file)",
    re.IGNORECASE,
)
_ARTIFACT_RESUME_RE = re.compile(
    r"^\s*(?:请\s*)?(?:继续|接着做|接着来|继续做|continue|resume|go\s+on)"
    r"(?:\s*(?:吧|一下))?\s*[。.!！？?~～]*\s*$",
    re.IGNORECASE,
)
_ARTIFACT_OUTPUT_PATTERNS = tuple(re.compile(pattern, re.IGNORECASE) for pattern in (
    # Require an output/delivery verb near a document noun.  Input size and a
    # noun such as "全文" alone describe what is being *read*, not the desired
    # output medium (e.g. a 20k-character source followed by "三句话总结全文").
    r"(?:生成|输出|导出|保存|写入|创建|制作|交付|整理成|汇总成).{0,16}"
    r"(?:(?:完整|详细|审校|校对|最终)){0,3}(?:报告|文档|文件|稿件|完整稿|白皮书|说明书|全文)",
    r"(?:报告|文档|稿件|白皮书|说明书|全文).{0,16}"
    r"(?:保存|写入|输出|导出|交付|下载|作为附件|成文件)",
    r"(?:给我|提供|交付).{0,10}(?:一份|一个)?.{0,8}"
    r"(?:(?:完整|详细|审校|校对)){0,3}(?:报告|文档|文件|稿件|白皮书|说明书)",
    r"(?:generate|create|write|export|save|deliver|produce).{0,40}"
    r"(?:full |complete |detailed |proofreading )?(?:report|document|file|manuscript)",
    r"(?:report|document|file|manuscript).{0,40}"
    r"(?:as (?:an? )?(?:attachment|file)|to (?:a )?file|for download)",
    # Preserve the pre-existing terse command form without reintroducing the
    # old broad substring match.  A bare "完整报告" is an output request;
    # "请用三句话概括下面的完整报告" is not.
    r"^\s*(?:请|请给我|给我|帮我|我要|需要|要)?\s*(?:一份|一个)?\s*"
    r"(?:审校报告|校对报告|完整报告|详细报告|完整稿)\s*[。！!？?]?\s*$",
    r"^\s*(?:please\s+)?(?:a\s+)?(?:full|complete|detailed|proofreading)\s+"
    r"(?:report|document|manuscript)\s*[.!?]?\s*$",
    # Bare "写/撰写" is too common in source narration ("我写的报告") to
    # use as an unanchored output verb. Restrict it to an imperative opening.
    # The bounded span is deliberately generous: a legitimate brief often puts
    # a long subject clause between the verb and the final "完整报告" noun.
    r"^\s*(?:请(?:你)?|请帮我|帮我|麻烦(?:你)?|给我|替我|为我)?\s*"
    r"(?:写|撰写)\s*(?:一份|一个)?\s*.{0,160}"
    r"(?:报告|文档|文件|稿件|白皮书|说明书)",
    r"^\s*(?:please\s+)?(?:help\s+me\s+)?"
    r"(?:write|create|generate|produce|prepare|draft)\s+.{0,200}"
    r"(?:report|document|file|manuscript)",
))
_DEICTIC_RE = re.compile(
    r"(上述|上面|前面|刚才|刚刚|这些|该项|照这个|按这个|"
    r"逐项|继续|接着|\babove\b|\bprevious\b|\bthose\b|\bcontinue\b|\bresume\b)",
    re.IGNORECASE,
)


def wants_resume(text: str) -> bool:
    """True when the user's message is essentially a bare continue command."""
    stripped = (text or "").strip().strip("。.!！~～ ")
    if not stripped or len(stripped) > 12:
        return False
    lowered = stripped.lower()
    return any(lowered.startswith(m) for m in _RESUME_MARKERS)


def artifact_output_required(
    text: str,
    available_names: set[str | None],
    artifact_resume_state: object = None,
    *,
    context_key: str = "",
    now: float | None = None,
) -> bool:
    """Resolve the artifact contract from this request and resumable state."""
    artifact_flow = {
        "artifact_create", "artifact_append", "artifact_validate",
        "artifact_finalize", "artifact_deliver",
    }
    resume_artifact = wants_artifact_resume(text) and artifact_continuation_is_live(
        artifact_resume_state, context_key=context_key, now=now,
    )
    return artifact_flow.issubset(available_names) and (
        ContextStage._expects_artifact(text) or resume_artifact
    )


def wants_artifact_resume(text: str) -> bool:
    """Artifact recovery accepts only a bare continue command.

    General plan continuation remains slightly permissive for compatibility;
    durable artifact state is side-effecting and therefore uses this stricter
    boundary so "继续讨论股票" can never revive an old report.
    """
    return bool(_ARTIFACT_RESUME_RE.fullmatch(text or ""))


def artifact_continuation_is_live(
    state: object,
    *,
    context_key: str = "",
    now: float | None = None,
) -> bool:
    """Validate that a resume marker belongs to this conversation epoch.

    Session metadata is durable, so mere dictionary presence is not enough: an
    abandoned report could otherwise hijack an unrelated bare "continue" days
    later.  New markers are versioned, tied to the source event and reset-bounded
    context key, and expire after a short recovery window.
    """
    if not isinstance(state, dict) or state.get("version") != _ARTIFACT_CONTINUATION_VERSION:
        return False
    if not str(state.get("trace_id") or "") or not str(state.get("source_event_id") or ""):
        return False
    state_context = str(state.get("context_key") or "")
    if not state_context or (context_key and state_context != context_key):
        return False
    try:
        updated_at = float(state.get("updated_at"))
    except (TypeError, ValueError):
        return False
    current = time.time() if now is None else now
    # Reject implausible future stamps as well as expired ones.  A one-minute
    # allowance avoids losing a resumable turn to tiny wall-clock adjustments.
    age = current - updated_at
    return -60.0 <= age <= _ARTIFACT_CONTINUATION_TTL_SECONDS


def _message_text(message: dict[str, Any]) -> str:
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(
            str(part.get("text", ""))
            for part in content
            if isinstance(part, dict) and part.get("type") == "text"
        )
    return str(content or "")


def contextual_retrieval_query(text: str, history: list[dict[str, Any]]) -> str:
    """Resolve deictic search terms from the immediate conversation.

    Queries like "apply the above changes" are nearly content-free on their
    own. Sending that text directly to long-term retrieval lets an unrelated
    old checklist become the strongest lexical match. For such turns only,
    append the recent human-visible exchange to the retrieval query; ordinary
    self-contained requests keep their historical behaviour and cost.
    """
    current = (text or "").strip()
    if not current or not _DEICTIC_RE.search(current):
        return current
    recent: list[str] = []
    for message in reversed(history):
        role = message.get("role")
        if role not in {"user", "assistant"} or message.get("tool_calls"):
            continue
        body = _message_text(message).strip()
        if body:
            recent.append(f"{role}: {body[-800:]}")
        if len(recent) >= 4:
            break
    if not recent:
        return current
    recent.reverse()
    return f"{' '.join(recent)} current user: {current}"[-2400:]


def planning_context(history: list[dict[str, Any]], retrieval: str) -> str:
    """Build planner context with conversation/recalled-data provenance.

    A configured PLAN_EXECUTE strategy runs a separate model call that does not
    otherwise see chat history. Passing only retrieval made it capable of
    resolving "the above" against old memory and injecting that stale plan into
    the real inference turn.
    """
    recent: list[str] = []
    for message in reversed(history):
        role = message.get("role")
        if role not in {"user", "assistant"} or message.get("tool_calls"):
            continue
        body = _message_text(message).strip()
        if body:
            recent.append(f"{role}: {body[-1000:]}")
        if len(recent) >= 4:
            break
    recent.reverse()
    parts: list[str] = []
    if recent:
        parts.append(
            "Recent conversation (authoritative for references such as "
            "'above' and 'continue'):\n" + "\n".join(recent)
        )
    if retrieval:
        parts.append(
            "Recalled background (may be stale; never redefine the active task):\n"
            + retrieval[:2000]
        )
    return "\n\n".join(parts)


def build_user_message_with_reply(event: InboundEvent) -> str:
    """构造写入会话历史的用户消息文本，把被引用消息原文作为前缀注入。

    跨通道统一的「理解层」：让模型知道用户在针对历史里哪一条消息发问（消歧），
    而不是只看到用户这次的新文字。无被引用原文时原样返回 event.text，不注入。
    注意只影响写入历史的副本，不改 event.text（检索/压缩仍用原始问题）。
    """
    reply_text = (event.reply_to_text or "").strip()
    if not reply_text:
        return event.text
    snippet = reply_text[:_REPLY_SNIPPET_MAX]
    if event.reply_to_is_own:
        prefix = f'[回复你刚才的消息: "{snippet}"]'
    elif event.reply_to_sender:
        prefix = f'[引用 {event.reply_to_sender}: "{snippet}"]'
    else:
        prefix = f'[引用: "{snippet}"]'
    return f"{prefix}\n\n{event.text}" if event.text else prefix


class ContextStage:
    """Builds the full pipeline context: system prompt, messages, retrieval, tool defs."""

    _TASK_MARKERS = {
        "document": (
            "审校", "校对", "报告", "文档", "全文", "完整稿", "白皮书", "说明书",
            "proofread", "report", "document", "manuscript", "full review",
        ),
        "code": ("代码", "报错", "bug", "函数", "class ", "def ", "typescript", "python"),
        "research": ("搜索", "查找", "search", "find", "look up", "查一下"),
        "planning": ("计划", "规划", "plan", "schedule", "安排"),
    }

    def __init__(
        self,
        *,
        config: Config,
        sessions: SessionManager,
        memory: MemoryStore,
        compressor: ConversationCompressor,
        context_builder: ContextBuilder,
        skill_store: SkillStore | None,
        knowledge: KnowledgeIndex | None,
        hybrid_retriever: HybridRetriever | None,
        planner: AgentPlanner | None,
        inference: InferenceController,
        working_memories: OrderedDict,
        memory_snapshots: OrderedDict,
        memory_snapshot_ids: "OrderedDict | None" = None,
        put_snapshot: "Callable[[str, str, frozenset], Awaitable[None]] | None" = None,
        memory_snapshot_meta: "dict[str, tuple[str, int]] | None" = None,
        scope_version_fn: "Callable[[str], int] | None" = None,
        snapshot_enabled: bool,
        memory_enabled: bool = True,
        tool_definitions_fn: Any,
        episodic: Any = None,
        narrative_episode_count: int = 3,
        plan_run_store: Any = None,
        retrieval_cache_get: "Callable[[str], Any] | None" = None,
        retrieval_on_miss: str = "degrade",
        retrieval_miss_timeout: float = 0.8,
        cache_ttl: float = 60.0,
        cache_jaccard_min: float = 0.3,
        cognitive_emitter: "CognitiveEmitter | None" = None,
    ):
        self._config = config
        self._sessions = sessions
        self._memory = memory
        self._compressor = compressor
        self._context_builder = context_builder
        self._skill_store = skill_store
        self._knowledge = knowledge
        self._hybrid_retriever = hybrid_retriever
        self._planner = planner
        self._inference = inference
        self._working_memories = working_memories
        self._memory_snapshots = memory_snapshots
        self._memory_snapshot_ids = memory_snapshot_ids if memory_snapshot_ids is not None else {}
        self._put_snapshot = put_snapshot
        self._memory_snapshot_meta = memory_snapshot_meta if memory_snapshot_meta is not None else {}
        self._scope_version_fn = scope_version_fn
        self._snapshot_enabled = snapshot_enabled
        self._memory_enabled = memory_enabled
        self._tool_definitions_fn = tool_definitions_fn
        self._episodic = episodic
        self._narrative_episode_count = narrative_episode_count
        self._plan_run_store = plan_run_store
        self._retrieval_cache_get = retrieval_cache_get
        self._retrieval_on_miss = retrieval_on_miss
        self._retrieval_miss_timeout = retrieval_miss_timeout
        self._cache_ttl = cache_ttl
        self._cache_jaccard_min = cache_jaccard_min
        self._cog = cognitive_emitter

    async def _emit_memory_recalled(self, event: InboundEvent, scored: list) -> None:
        """Emit a `memory_recalled` cognitive frame carrying each recalled
        memory's content/source-grade/score. Called from build() after the
        turn's memory items are known.

        Normalizes BOTH shapes deliberately: the real call site passes
        `(entry, score)` tuples (entry has `.content`/`.source`), while unit
        tests and any structured-dict caller pass `{"content","source","score"}`
        dicts. Handling both keeps the emitter robust to either provenance
        without a shared type dependency — not overbuilding.
        """
        # Gate before building items: on IM channels this skips the whole
        # slice/round loop, not just a discarded emit() payload.
        if self._cog is None or not scored or not self._cog.active(event):
            return
        items: list[dict[str, Any]] = []
        for s in scored[:12]:
            if isinstance(s, dict):
                content = s.get("content", "")
                source = s.get("source", "legacy")
                score = s.get("score", 0.0)
            elif isinstance(s, tuple):  # real call site: (entry, score)
                entry, score = s[0], (s[1] if len(s) > 1 else 0.0)
                content = getattr(entry, "content", "")
                source = getattr(entry, "source", "legacy")
            else:  # bare entry object
                content = getattr(s, "content", str(s))
                source = getattr(s, "source", "legacy")
                score = getattr(s, "score", 0.0)
            items.append({
                "content": str(content)[:200],
                "source": source or "legacy",
                "score": round(float(score or 0.0), 3),
            })
        await self._cog.emit(
            event, "memory_recalled", {"items": items},
            f"召回 {len(items)} 条记忆",
        )

    async def _fetch_knowledge(self, query: str, user_id: str, *, channel: str = "") -> tuple[list, str]:
        """Inline knowledge retrieval. Vector path is async; keyword-only path
        degrades internally. Scoped by user_id for access control."""
        results = await self._knowledge.search_async(
            query, limit=self._config.knowledge.max_results, user_id=user_id, channel=channel
        )
        context = self._knowledge.format_results(results)
        return results, context

    async def _bounded_retrieve(
        self, event: InboundEvent, context_key: str = "", query: str = "",
    ) -> list | None:
        """Degrade-mode cache miss: sync retrieval under a time budget.

        Latency-first CLI still deserves memory on first turns and topic
        switches — those are exactly the misses. Budget exceeded → local
        keyword search (fast, no embedding call). Budget 0 → skip entirely
        (the legacy degrade), keeping the old escape hatch configurable.
        """
        import asyncio

        timeout = self._retrieval_miss_timeout
        if timeout <= 0 or not self._hybrid_retriever:
            if timeout > 0 and not self._hybrid_retriever:
                # No retriever wired (vector off): keyword search IS the
                # bounded path, and it's synchronous/fast already.
                # 可见性用 memory_scope(owner-aware),与写侧 source_session 对齐。
                # audience=RETRIEVAL:兜底召回本就是"该显示的召回",与 Hybrid
                # 主路径对齐,过滤 superseded/archived/unresolved,不漏进 prompt。
                return self._memory.search_scored(
                    query or event.text, limit=5, session_key=event.memory_scope,
                    audience=Audience.RETRIEVAL,
                )
            return None
        try:
            return await asyncio.wait_for(
                self._hybrid_retriever.retrieve(
                    query or event.text, limit=8,
                    memory_scope=event.memory_scope,
                    episode_session_key=context_key or event.session_key,
                ),
                timeout=timeout,
            )
        except asyncio.TimeoutError:
            logger.debug(
                "Bounded retrieval timed out after {}s; keyword fallback", timeout
            )
            try:
                return self._memory.search_scored(
                    query or event.text, limit=5, session_key=event.memory_scope,
                    audience=Audience.RETRIEVAL,
                )
            except Exception as e:
                logger.debug("Keyword fallback failed: {}", e)
                return None
        except Exception as e:
            logger.debug("Bounded retrieval failed: {}", e)
            return None

    async def build(
        self,
        event: InboundEvent,
        session: Session,
        *,
        publish_response: bool,
        trace_id: str,
        stream_publisher: Any,
        intro_text: str,
    ) -> PipelineContext:
        from echo_agent.session.context_epoch import conversation_context_key

        context_key = conversation_context_key(event.session_key, session)
        ephemeral = _is_ephemeral_session(event.session_key, event.channel)
        working_ctx = ""
        if context_key in self._working_memories:
            working_ctx = self._working_memories[context_key].get_context()

        snapshot_ids: frozenset[str] = frozenset()
        if not self._memory_enabled:
            # 总开关关闭：不读任何长期/快照记忆，只保留本轮 working memory。
            memory_ctx = working_ctx
        elif self._snapshot_enabled and not ephemeral:
            cur_ver = self._scope_version_fn(event.memory_scope) if self._scope_version_fn else 0
            meta = self._memory_snapshot_meta.get(context_key)
            snapshot_valid = (
                context_key in self._memory_snapshots
                and meta is not None
                and meta == (event.memory_scope, cur_ver)
            )
            if snapshot_valid:
                snapshot = self._memory_snapshots[context_key]
                snapshot_ids = self._memory_snapshot_ids.get(context_key, frozenset())
            else:
                # R3 叙事层:async 上下文预取最近 N 条 episode.summary 传入,规避
                # get_snapshot_with_ids 转 async 牵动全部调用点。叙事随快照一起
                # 缓存,scope 版本 bump 时随快照失效(属既有缓存范畴)。
                narrative_summaries: list[str] = []
                if self._episodic is not None and self._narrative_episode_count > 0:
                    try:
                        episodes = await self._episodic.get_session_episodes(
                            context_key, self._narrative_episode_count
                        )
                        narrative_summaries = [e.summary for e in episodes if e.summary]
                    except Exception as e:
                        logger.debug("Narrative episode prefetch failed: {}", e)
                snapshot, snapshot_ids = self._memory.get_snapshot_with_ids(
                    session_key=event.memory_scope,
                    episode_summaries=narrative_summaries or None,
                )
                if self._put_snapshot is not None:
                    # 写入唯一入口经 loop 的统一 LRU(锁 + 上限);
                    # put_snapshot 为空时本轮仍用刚算出的 snapshot,但不缓存
                    # (不得回退到无界直写 dict)。记录构建时 (scope, version),
                    # 该 scope 被写后 bump 版本即令此快照失效。
                    await self._put_snapshot(
                        context_key, snapshot, snapshot_ids,
                        event.memory_scope, cur_ver,
                    )
            memory_ctx = build_memory_context(
                self._memory,
                snapshot=snapshot,
                working_memory=working_ctx,
                allow_env_writes=self._config.memory.allow_model_environment_writes,
            )
        elif ephemeral:
            # eval/test：不读任何长期/快照记忆,只保留本轮 working memory。
            memory_ctx = working_ctx
        else:
            memory_ctx = build_memory_context(
                self._memory,
                session_key=event.memory_scope,
                working_memory=working_ctx,
                allow_env_writes=self._config.memory.allow_model_environment_writes,
            )

        skills_ctx = build_skills_context(self._skill_store)
        # Derive capabilities from the live tool registry (config, not memory).
        tool_defs = self._inference.filter_tools(self._tool_definitions_fn(channel=event.channel))
        capabilities_ctx = build_capabilities_context(tool_defs)
        system_prompt = self._context_builder.build_system_prompt(
            memory_context=memory_ctx,
            skills_context=skills_ctx,
            capabilities=capabilities_ctx,
            channel=event.channel,
        )

        history = session.get_history(self._config.session.max_history_messages)
        if self._compressor.should_compress(history):
            self._compressor._session_key = context_key
            history_copy = copy.deepcopy(history)
            result = await self._compressor.compress(history_copy, focus_topic=event.text)
            history = result.messages
            if result.was_compressed:
                logger.info(
                    "Context compressed: {} → {} tokens",
                    result.tokens_before,
                    result.tokens_after,
                )
                session.messages = session.messages[:session.last_consolidated] + result.messages
                await self._sessions.save(session)

        # Resolve an explicit reply quote before retrieval as well as inference:
        # "apply this" is otherwise as content-free to BM25/vector search as an
        # unquoted "above", even though the channel supplied the exact referent.
        user_message = build_user_message_with_reply(event)
        retrieval_query = contextual_retrieval_query(user_message, history)

        media_items = event.media_items
        resolved_media = (
            await self._context_builder.resolve_inbound_media(media_items, event.channel)
            if media_items
            else None
        )

        media_refs = self._build_media_refs(resolved_media) if resolved_media else None
        # 引用回复：把被引用消息原文作为前缀注入写入历史的文本，供模型消歧。
        # 只影响历史副本，event.text 保持原样（上游检索/压缩仍用原始问题）。
        if media_refs:
            session.add_message("user", user_message, media_refs=media_refs)
        else:
            session.add_message("user", user_message)

        retrieval_parts: list[str] = []
        from echo_agent.memory.prefetch import is_fresh

        # A single per-session prefetched entry warms main memory + episodic +
        # knowledge together (Task 13). Fetch it once here so all three segments
        # below share one freshness decision. Knowledge lives outside the
        # memory.enabled block, so the lookup must sit above it.
        cached = (
            self._retrieval_cache_get(context_key)
            if self._retrieval_cache_get is not None
            else None
        )
        cur_ver = self._scope_version_fn(event.memory_scope) if self._scope_version_fn else 0
        cache_fresh = (
            cached is not None
            and getattr(cached, "scope", "") == event.memory_scope
            and getattr(cached, "scope_version", 0) == cur_ver
            and is_fresh(
                cached, retrieval_query, now=time.time(),
                ttl=self._cache_ttl, jaccard_min=self._cache_jaccard_min,
            )
        )
        if self._config.memory.enabled and not ephemeral:
            # Prefer a fresh prefetched result (zero inline latency). On a miss,
            # `retrieval_on_miss` decides: "sync" pays full retrieval latency
            # this turn (accuracy-first daemon/gateway); "degrade" runs a
            # BOUNDED sync retrieval — a short time budget, falling back to
            # local keyword search on timeout. First turns and topic switches
            # are exactly when retrieval matters most; skipping entirely (the
            # old degrade) made memory silently unavailable on those turns.
            scored = None
            if cache_fresh:
                scored = cached.scored
            elif self._retrieval_on_miss == "sync":
                if self._hybrid_retriever:
                    # Episode candidates are assembled inside retrieve() by
                    # relevance (semantic + LIKE), same as the prefetch path —
                    # no separate "recent N" fetch here, which would otherwise
                    # miss high-relevance episodes outside the recency window.
                    scored = await self._hybrid_retriever.retrieve(
                        retrieval_query, limit=8,
                        memory_scope=event.memory_scope,
                        episode_session_key=context_key,
                    )
                else:
                    scored = self._memory.search_scored(
                        retrieval_query, limit=5, session_key=event.memory_scope,
                        audience=Audience.RETRIEVAL,
                    )
            else:
                scored = await self._bounded_retrieve(
                    event, context_key, retrieval_query,
                )
            if scored:
                scored = filter_recall_by_snapshot(scored, snapshot_ids)
            if scored:
                from echo_agent.memory.types import Episode as _Ep
                mem_items = [(r, s) for r, s in scored if not isinstance(r, _Ep)]
                ep_items = [(r, s) for r, s in scored if isinstance(r, _Ep)]
                if mem_items:
                    retrieval_parts.append(
                        "Relevant memory:\n"
                        + "\n".join(f"- {r.key}: {r.content}" for r, _ in mem_items)
                    )
                    try:
                        self._memory.reinforce([r.id for r, _ in mem_items])
                    except Exception as e:
                        logger.debug("Memory reinforcement failed: {}", e)
                if ep_items:
                    retrieval_parts.append(
                        "Past episodes:\n"
                        + "\n".join(f"- {r.summary}" for r, _ in ep_items if r.summary)
                    )
                # Cognitive埋点: surface the actual recalled memories (content/
                # source-grade/score) to the CLI TUI. Pass mem_items (memory
                # entries only), NOT `scored` which mixes in episodes. The
                # emitter internally gates to channel=="gateway:cli".
                if mem_items and self._cog is not None:
                    await self._emit_memory_recalled(event, mem_items)

        if self._knowledge:
            # Knowledge is ACL-filtered per user (KnowledgeIndex.search filters
            # by allowed_users). A cached knowledge_context is only trustworthy
            # when it was prefetched for THIS turn's user: under a shared group
            # session_key the cache entry is reachable by every sender, so
            # serving user A's ACL-filtered knowledge to user B would leak
            # restricted docs. So the cache hit requires knowledge_user_id to
            # match the current sender.
            knowledge_context = None
            cached_user_ok = (
                cache_fresh
                and cached.knowledge_context is not None
                and bool(event.sender_id)
                and cached.knowledge_user_id == event.sender_id
            )
            if cached_user_ok:
                knowledge_context = cached.knowledge_context
            else:
                # Miss (no/stale cache, or knowledge was prefetched for another
                # user). The scan is CPU-bound, so any inline fetch runs in an
                # executor thread and never blocks the event loop. When a
                # prefetcher is warming knowledge (hybrid retriever present),
                # honor retrieval_on_miss: "sync" fetches inline now, "degrade"
                # skips and lets the next prefetch warm it. When there is NO
                # prefetcher (memory disabled -> no hybrid retriever), nothing
                # will ever warm the cache, so a degrade-skip would silently
                # drop knowledge every turn — fall back to inline so knowledge
                # keeps working independently of memory.enabled (its pre-Task-13
                # behavior).
                #
                # Senderless entrypoints (sender_id == "") can never satisfy the
                # cache-hit guard above (it requires a non-empty sender to avoid
                # leaking one user's ACL-filtered knowledge to another under a
                # shared session_key). For them the prefetch is unusable, so a
                # degrade-skip would drop knowledge on every turn. Treat the
                # prefetch as inactive when there is no sender and fall back to
                # inline: the inline fetch passes the empty user_id, which the
                # index resolves to public (unrestricted) docs only — no leak.
                knowledge_prefetch_active = (
                    self._hybrid_retriever is not None and bool(event.sender_id)
                )
                if self._retrieval_on_miss == "sync" or not knowledge_prefetch_active:
                    try:
                        _, knowledge_context = (
                            await self._fetch_knowledge(
                                retrieval_query, event.sender_id, channel=event.channel,
                            )
                        )
                    except Exception as e:
                        logger.debug("Knowledge retrieval failed: {}", e)
            if knowledge_context:
                retrieval_parts.append(knowledge_context)

        task_type = self._infer_task_type(event.text)

        retrieval = "\n\n".join(retrieval_parts)

        session_cfg = self._config.session
        messages = self._context_builder.build_messages(
            history=history,
            # 引用回复:本轮 prompt 也用带引用前缀的 user_message,让模型当轮就看到
            # 被引用原文(消歧);event.text 保持原样,上游检索/压缩仍用原始问题。
            current_message=user_message,
            media=resolved_media,
            channel=event.channel,
            chat_id=event.chat_id,
            system_prompt=system_prompt,
            retrieval_context=retrieval,
            history_image_ttl_minutes=session_cfg.history_image_ttl_minutes,
            history_image_limit=session_cfg.history_image_limit,
            history_image_skip_if_current=session_cfg.history_image_skip_if_current,
        )

        available_names = {
            item.get("function", {}).get("name") for item in tool_defs if isinstance(item, dict)
        }
        artifact_resume_state = (
            session.metadata.get("_artifact_continuation")
            if isinstance(session.metadata, dict) else None
        )
        resume_artifact = wants_artifact_resume(event.text) and artifact_continuation_is_live(
            artifact_resume_state, context_key=context_key,
        )
        if isinstance(artifact_resume_state, dict) and not resume_artifact:
            # Any new request supersedes the failed report.  Invalid/expired
            # state is also removed eagerly so a later bare "continue" cannot
            # revive it if the current turn fails before inference cleanup.
            session.metadata.pop("_artifact_continuation", None)
            artifact_resume_state = None
        artifact_required = artifact_output_required(
            event.text,
            available_names,
            artifact_resume_state,
            context_key=context_key,
        )
        artifact_intent_id = ""
        if artifact_required:
            artifact_intent_id = str(event.event_id or "")
            if resume_artifact and isinstance(artifact_resume_state, dict):
                artifact_intent_id = str(
                    artifact_resume_state.get("source_event_id") or artifact_intent_id
                )
        if artifact_required:
            if resume_artifact:
                instruction = (
                    "[Output mode: artifact resume] The previous artifact workflow did not reach "
                    "successful delivery. Resume from the artifact IDs and tool results already in "
                    "conversation history; do not create a duplicate when a usable draft exists. "
                    "Append one chunk per turn, validate, finalize, and deliver it."
                )
            else:
                instruction = (
                    "[Output mode: artifact] This request requires a complete long-form document. "
                    "Create it with the artifact tools in ordered chunks, validate and finalize it, "
                    "then deliver it. Keep the final chat answer to a short summary and delivery status; "
                    "do not paste the full document into one model response."
                )
            last_content = messages[-1]["content"]
            if isinstance(last_content, list):
                last_content[0]["text"] += f"\n\n{instruction}"
            else:
                messages[-1]["content"] = f"{last_content}\n\n{instruction}"

        continuation_state = (
            session.metadata.get("_output_continuation")
            if isinstance(session.metadata, dict) else None
        )
        if (
            wants_resume(event.text)
            and not artifact_required
            and isinstance(continuation_state, dict)
        ):
            tail = str(continuation_state.get("tail") or "")[-2000:]
            resume_instruction = (
                "[Output continuation — resumed] The previous final answer was truncated. "
                "Continue exactly after the saved tail below without repeating it, and finish the answer.\n"
                f"<saved_tail>{tail}</saved_tail>"
            )
            last_content = messages[-1]["content"]
            if isinstance(last_content, list):
                last_content[0]["text"] += f"\n\n{resume_instruction}"
            else:
                messages[-1]["content"] = f"{last_content}\n\n{resume_instruction}"

        # tool_defs already computed above for capability derivation.

        execution_plan = None
        plan_run_id = ""
        if self._planner and tool_defs:
            # Resume path: an interrupted multi-step plan (exhausted iterations
            # / budget halt / crash) + a bare "continue" from the user picks up
            # the stored run instead of planning from scratch. Previously
            # get_resumable() had no production caller — interrupted plans were
            # persisted but every next message re-planned.
            if self._plan_run_store is not None and wants_resume(event.text):
                try:
                    resumable = await self._plan_run_store.get_resumable(
                        context_key
                    )
                except Exception as e:
                    logger.debug("Resumable plan lookup failed: {}", e)
                    resumable = None
                if resumable is not None:
                    plan_run_id, execution_plan = resumable
                    plan_context = execution_plan.to_prompt()
                    last_content = messages[-1]["content"]
                    resume_note = (
                        "[Plan — resumed]\n以下是上一轮未完成的计划及进度,"
                        f"请从中断处继续:\n{plan_context}"
                    )
                    if isinstance(last_content, list):
                        last_content[0]["text"] += f"\n\n{resume_note}"
                    else:
                        messages[-1]["content"] = f"{last_content}\n\n{resume_note}"
                    logger.info(
                        "Resuming plan run {} for session {}",
                        plan_run_id, context_key,
                    )
        if self._planner and tool_defs and execution_plan is None:
            try:
                token_est = len(user_message) // 4
                execution_plan = await self._planner.create_plan(
                    query=user_message,
                    tools=tool_defs,
                    context=planning_context(history, retrieval),
                    token_estimate=token_est,
                )
                # A single-step plan ("reason and act iteratively") carries no
                # information — injecting it just burns prompt tokens.
                if execution_plan and len(execution_plan.steps) > 1:
                    plan_context = execution_plan.to_prompt()
                    last_content = messages[-1]["content"]
                    if isinstance(last_content, list):
                        last_content[0]["text"] += f"\n\n[Plan]\n{plan_context}"
                    else:
                        messages[-1]["content"] = (
                            last_content + f"\n\n[Plan]\n{plan_context}"
                        )
                    # Persist the multi-step plan so step progress is queryable
                    # and an interrupted long task can be resumed.
                    if self._plan_run_store is not None:
                        try:
                            plan_run_id = await self._plan_run_store.create(
                                context_key, trace_id, execution_plan
                            )
                        except Exception as e:
                            logger.debug("Plan run persistence failed: {}", e)
            except Exception as e:
                logger.debug("Planning failed, proceeding without plan: {}", e)

        return PipelineContext(
            event=event,
            session=session,
            trace_id=trace_id,
            publish_response=publish_response,
            context_key=context_key,
            system_prompt=system_prompt,
            messages=messages,
            tool_defs=tool_defs,
            retrieval=retrieval,
            task_type=task_type,
            artifact_required=artifact_required,
            artifact_intent_id=artifact_intent_id,
            execution_plan=execution_plan,
            plan_run_id=plan_run_id,
            intro_text=intro_text,
            stream_publisher=stream_publisher,
        )

    @staticmethod
    def _build_media_refs(resolved_media: list[dict[str, str]]) -> list[dict[str, Any]]:
        """Extract lightweight image references from resolved media for session storage."""
        import time

        from echo_agent.session.media_ref import MediaRef

        refs: list[dict[str, Any]] = []
        now = time.time()
        for item in resolved_media:
            if item.get("type") != "image":
                continue
            url = item.get("url", "")
            if not url:
                continue
            is_local = not url.startswith(("http://", "https://", "data:"))
            refs.append(MediaRef(
                cache_path=url if is_local else "",
                original_url=item.get("original_url", "") or (url if not is_local else ""),
                mime_type=item.get("mime_type", ""),
                timestamp=now,
                aes_key=item.get("aes_key", ""),
            ).to_dict())
        return refs

    def _infer_task_type(self, text: str) -> str:
        lower = text.lower()
        for task_type, markers in self._TASK_MARKERS.items():
            if any(marker in lower for marker in markers):
                return task_type
        return "chat"

    @staticmethod
    def _expects_artifact(text: str) -> bool:
        lower = (text or "").lower()
        if any(marker in lower for marker in _ARTIFACT_NO_FILE_MARKERS):
            return False
        # "直接在聊天回答" suppresses file output; "不要在聊天回答"
        # requests the opposite. Inspect local polarity instead of treating the
        # same substring as both meanings.
        for match in _ARTIFACT_CHAT_OUTPUT_RE.finditer(lower):
            prefix = lower[max(0, match.start() - 12):match.start()]
            if not _ARTIFACT_CHAT_NEGATION_RE.search(prefix):
                return False
        if _ARTIFACT_HOW_TO_RE.search(lower):
            return False
        if _ARTIFACT_DOC_TRANSFORM_RE.search(lower):
            return False
        return any(pattern.search(lower) for pattern in _ARTIFACT_OUTPUT_PATTERNS)
