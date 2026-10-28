"""IM 通道追问续接(follow-up continuation)测试。

覆盖三层:
1. ClarifyManager 的 session_key 维度 IM pending 注册 / 取用 / TTL 过期;
2. AgentLoop._maybe_bind_im_clarify_answer 把待答问题绑定到下一条消息;
3. 与现有引用注入(build_user_message_with_reply)串联后写入历史的文本形态。
"""

import time

import pytest

from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.loop import AgentLoop
from echo_agent.agent.pipeline.context_stage import build_user_message_with_reply
from echo_agent.bus.events import EventType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider, LLMResponse


class _StubProvider(LLMProvider):
    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok", finish_reason="stop")

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        return await self.chat(messages, tools, model, tool_choice, **kwargs)

    def get_default_model(self):
        return "stub"


# /***********************************************************
#  *                    钉钉追问投递回归                      *
# /***********************************************************/

class _QuestionProvider(_StubProvider):
    """模拟截图中的模型调用；仅模型边界替身，执行真实提问工具和消息总线。"""

    def __init__(self, options):
        super().__init__()
        self.calls = 0
        self.options = options

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        from echo_agent.models.provider import ToolCallRequest
        self.calls += 1
        if self.calls == 1:
            return LLMResponse(
                content="可以，拉取不影响使用已有素材创建任务。",
                tool_calls=[ToolCallRequest(id="question", name="clarify", arguments={
                    "question": "你想按什么条件筛选？", "options": self.options,
                })], finish_reason="tool_calls",
            )
        return LLMResponse(content="问题已发给你，回一个字母即可。", finish_reason="stop")


@pytest.mark.asyncio
@pytest.mark.parametrize("options", [["按平台筛选", "按热度排序"], ["A. 按平台筛选", "B. 按热度排序"],
                                     []])
@pytest.mark.parametrize("delivery_success", [True, False])
async def test_dingtalk_clarify_reaches_outbound_with_answer_and_options(tmp_path, options, delivery_success):
    """真实 AgentLoop 出站必须带上解释、问题和选项，不能只投递已发送宣称。"""
    from echo_agent.channels.base import SendResult
    provider = _QuestionProvider(options)
    config = load_config(overrides={"workspace": str(tmp_path), "memory": {"enabled": False}})
    bus = MessageBus()
    loop = AgentLoop(bus=bus, config=config, provider=provider, workspace=tmp_path)
    loop._running = True
    sent = []

    async def receive(event):
        if event.is_final:
            sent.append(event)
        return SendResult(success=delivery_success)

    bus.subscribe_outbound("dingtalk", receive)
    await loop._on_inbound(InboundEvent.text_message(
        channel="dingtalk", chat_id="staff", sender_id="staff",
        text="现在可以从已有列表筛选创建新书名任务吗？", event_id="question-event",
    ))
    assert len(sent) == 1
    text = sent[0].text
    assert "可以，拉取不影响" in text
    assert "你想按什么条件筛选？" in text
    if options:
        assert "A. 按平台筛选" in text and "B. 按热度排序" in text
        assert "A. A." not in text and "B. B." not in text
    assert "问题已发给你" not in text
    assert provider.calls == 1
    pending = loop.clarify._im_pending.get("dingtalk:staff")
    if not delivery_success:
        assert pending is None  # 未发出的提问不能占用下一条用户消息。
        return
    assert pending is not None and pending.options == options
    answer = InboundEvent.text_message(
        channel="dingtalk", chat_id="staff", sender_id="staff",
        text="b" if options else "番茄热度前10", event_id="answer-event",
    )
    await loop._on_inbound(answer)
    assert provider.calls == 2
    assert "你想按什么条件筛选？" in answer.reply_to_text
    if options:
        assert "B. 按热度排序" in answer.reply_to_text
        assert "B. B." not in answer.reply_to_text
    assert loop.clarify.take_im_pending("dingtalk:staff", ttl_seconds=300) is None


def _make_loop(tmp_path):
    config = load_config(overrides={"workspace": str(tmp_path)})
    bus = MessageBus()
    return AgentLoop(bus=bus, config=config, provider=_StubProvider(), workspace=tmp_path)


# ── ClarifyManager: IM pending registry ──────────────────────────────────────

def test_im_pending_register_and_take():
    mgr = ClarifyManager()
    mgr.register_im_pending("weixin:u1", "选哪个方案?", ["甲", "乙"], user_id="u1")
    req = mgr.take_im_pending("weixin:u1", ttl_seconds=300)
    assert req is not None
    assert req.question == "选哪个方案?"
    assert req.options == ["甲", "乙"]


def test_im_pending_is_consumed_once():
    mgr = ClarifyManager()
    mgr.register_im_pending("weixin:u1", "q", [])
    assert mgr.take_im_pending("weixin:u1", ttl_seconds=300) is not None
    # 取用即弹出,第二次取不到,避免同一待答被重复绑定。
    assert mgr.take_im_pending("weixin:u1", ttl_seconds=300) is None


def test_im_pending_isolated_by_session():
    mgr = ClarifyManager()
    mgr.register_im_pending("weixin:u1", "q1", [])
    assert mgr.take_im_pending("weixin:u2", ttl_seconds=300) is None
    assert mgr.take_im_pending("weixin:u1", ttl_seconds=300) is not None


def test_im_pending_latest_overwrites():
    mgr = ClarifyManager()
    mgr.register_im_pending("weixin:u1", "旧问题", [])
    mgr.register_im_pending("weixin:u1", "新问题", [])
    req = mgr.take_im_pending("weixin:u1", ttl_seconds=300)
    assert req.question == "新问题"


def test_im_pending_expires_by_ttl():
    mgr = ClarifyManager()
    mgr.register_im_pending("weixin:u1", "q", [])
    # 手动把创建时间调到很久以前,模拟超过 TTL。
    mgr._im_pending["weixin:u1"].created_at = time.monotonic() - 999
    assert mgr.take_im_pending("weixin:u1", ttl_seconds=300) is None
    # 过期项应已被清除,不残留。
    assert "weixin:u1" not in mgr._im_pending


def test_im_pending_does_not_touch_cli_registry():
    mgr = ClarifyManager()
    req = mgr.request("cli 问题", ["A"], session_key="gateway:cli:c")
    mgr.register_im_pending("weixin:u1", "im 问题", [])
    # IM 取用不影响 CLI 的 id-keyed pending(阻塞路径)。
    mgr.take_im_pending("weixin:u1", ttl_seconds=300)
    assert mgr.get(req.id) is not None


# ── AgentLoop._maybe_bind_im_clarify_answer ──────────────────────────────────

def _inbound(text, session_key="weixin:u1", **kwargs):
    return InboundEvent.text_message(
        channel="weixin", chat_id="u1", sender_id="u1", text=text,
        session_key_override=session_key, **kwargs,
    )


def test_bind_injects_question_as_own_reply(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "要部署到哪个环境?", ["预发", "生产"])
    event = _inbound("A")
    loop._maybe_bind_im_clarify_answer(event)
    assert event.reply_to_is_own is True
    assert "要部署到哪个环境?" in (event.reply_to_text or "")
    assert "A. 预发" in event.reply_to_text
    assert "B. 生产" in event.reply_to_text
    # 用户原始输入不被改写,检索/历史仍拿到裸 "A"。
    assert event.text == "A"


def test_bind_renders_into_history_prefix(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "选哪个?", ["预发", "生产"])
    event = _inbound("A")
    loop._maybe_bind_im_clarify_answer(event)
    # 复用引用注入:写入历史的文本带「回复你刚才的消息」前缀 + 用户答案。
    rendered = build_user_message_with_reply(event)
    assert "回复你刚才的消息" in rendered
    assert "选哪个?" in rendered
    assert rendered.rstrip().endswith("A")


def test_bind_open_ended_answer_without_options(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "你指的是哪个服务?", [])
    event = _inbound("订单服务")
    loop._maybe_bind_im_clarify_answer(event)
    assert event.reply_to_text == "你指的是哪个服务?"
    assert event.reply_to_is_own is True


def test_bind_noop_when_no_pending(tmp_path):
    loop = _make_loop(tmp_path)
    event = _inbound("A")
    loop._maybe_bind_im_clarify_answer(event)
    assert event.reply_to_text is None


def test_bind_noop_when_expired(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "q", ["A"])
    loop.clarify._im_pending["weixin:u1"].created_at = time.monotonic() - 999
    event = _inbound("A")
    loop._maybe_bind_im_clarify_answer(event)
    # 过期退回普通消息:不注入,交由模型自行理解(P2 提示词兜底)。
    assert event.reply_to_text is None


def test_bind_respects_explicit_user_quote(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "追问", ["A"])
    event = _inbound("A", reply_to_text="用户自己引用的另一条消息")
    loop._maybe_bind_im_clarify_answer(event)
    # 用户显式引用优先,不被隐式续接覆盖。
    assert event.reply_to_text == "用户自己引用的另一条消息"
    # 但残留的待答须被清理:引用回答成功后,下一条无关消息不应再被绑到旧问题。
    assert loop.clarify.take_im_pending("weixin:u1", ttl_seconds=300) is None


def test_bind_skips_cron_events(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "追问", ["A"])
    # 定时任务复用同一 session_key,不应消费待答:既不改写文本,也不弹出 pending。
    cron = _inbound("定时触发的内容", event_type=EventType.CRON, unattended=True)
    loop._maybe_bind_im_clarify_answer(cron)
    assert cron.reply_to_text is None
    # 待答仍在,留给真正的用户消息消费。
    assert loop.clarify.take_im_pending("weixin:u1", ttl_seconds=300) is not None


def test_bind_skips_unattended_and_control_events(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "追问", ["A"])
    unattended = _inbound("无人值守", unattended=True)
    loop._maybe_bind_im_clarify_answer(unattended)
    assert unattended.reply_to_text is None
    control = _inbound("控制事件", is_control=True)
    loop._maybe_bind_im_clarify_answer(control)
    assert control.reply_to_text is None
    assert loop.clarify.take_im_pending("weixin:u1", ttl_seconds=300) is not None


def test_bind_noop_when_clarify_missing(tmp_path):
    # __new__ 绕过 __init__ 的轻量构造路径不接线 self.clarify:必须安全 no-op,
    # 而非抛 AttributeError(否则会打红所有走该构造路径的既有测试)。
    loop = AgentLoop.__new__(AgentLoop)
    event = _inbound("A")
    loop._maybe_bind_im_clarify_answer(event)
    assert event.reply_to_text is None


def test_bind_consumes_pending_once(tmp_path):
    loop = _make_loop(tmp_path)
    loop.clarify.register_im_pending("weixin:u1", "q", ["A"])
    first = _inbound("A")
    loop._maybe_bind_im_clarify_answer(first)
    assert first.reply_to_text is not None
    # 第二条消息不应再被绑定到同一个已消费的待答。
    second = _inbound("B")
    loop._maybe_bind_im_clarify_answer(second)
    assert second.reply_to_text is None
