"""IM 通道追问续接(follow-up continuation)测试。

覆盖三层:
1. ClarifyManager 的 session_key 维度 IM pending 注册 / 取用 / TTL 过期;
2. AgentLoop._maybe_bind_im_clarify_answer 把待答问题绑定到下一条消息;
3. 与现有引用注入(build_user_message_with_reply)串联后写入历史的文本形态。
"""

import time

from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.loop import AgentLoop
from echo_agent.agent.pipeline.context_stage import build_user_message_with_reply
from echo_agent.bus.events import InboundEvent
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
