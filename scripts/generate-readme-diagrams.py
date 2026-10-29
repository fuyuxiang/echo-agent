#!/usr/bin/env python3
"""Build the bilingual, self-contained SVG diagrams used by the project README.

Run from any directory: python3 scripts/generate-readme-diagrams.py [--check]
No third-party packages, network, fonts, or raster images are embedded.

These are explanatory views, not diagrams automatically inferred from code.
When changing topology, review these authoritative implementations first:
  architecture: echo_agent/app.py; client/src-tauri/{Cargo.toml,src/agent_runtime.rs}
  runtime: echo_agent/{app.py,bus/queue.py,agent/loop.py,agent/pipeline/}
  learning: echo_agent/memory/{types,retrieval,eligibility,consolidator}.py;
            echo_agent/evolution/{engine,gate,validation}.py; config/schema.py
Python paths above are relative to echo-agent-runtime/.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "docs" / "assets" / "readme"
NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)

INK = "#142b49"
MUTED = "#526780"
BLUE = "#2563eb"
TEAL = "#087f82"
LINE = "#c8d8e9"


def el(parent: ET.Element, tag: str, **attrs: object) -> ET.Element:
    return ET.SubElement(parent, f"{{{NS}}}{tag}", {k.replace("_", "-"): str(v) for k, v in attrs.items()})


class Diagram:
    def __init__(self, height: int, title: str, subtitle: str, description: str):
        self.root = ET.Element(f"{{{NS}}}svg", {
            "width": "1200", "height": str(height), "viewBox": f"0 0 1200 {height}",
            "role": "img", "aria-labelledby": "title description",
        })
        el(self.root, "title", id="title").text = title
        el(self.root, "desc", id="description").text = description
        defs = el(self.root, "defs")
        for name, color in (("blue", BLUE), ("teal", TEAL), ("muted", MUTED)):
            marker = el(defs, "marker", id=name, markerWidth=8, markerHeight=8,
                        refX=7, refY=4, orient="auto-start-reverse", markerUnits="userSpaceOnUse")
            el(marker, "path", d="M0 0 8 4 0 8Z", fill=color)
        self.canvas = el(self.root, "g", font_family="-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Microsoft YaHei', Arial, sans-serif")
        self.rect(1, 1, 1198, height - 2, "#f7faff", LINE, 20)
        self.rect(36, 32, 5, 55, BLUE, "none", 2)
        self.text(56, 57, title, 30, INK, 700)
        self.text(56, 87, subtitle, 18, MUTED)

    def rect(self, x, y, width, height, fill="#ffffff", stroke=LINE, radius=12):
        return el(self.canvas, "rect", x=x, y=y, width=width, height=height,
                  rx=radius, fill=fill, stroke=stroke, stroke_width=1.2)

    def text(self, x, y, content, size=20, color=INK, weight=400, anchor="start"):
        node = el(self.canvas, "text", x=x, y=y, fill=color, font_size=size,
                  font_weight=weight, text_anchor=anchor)
        node.text = content
        return node

    def node(self, x, y, width, height, title, body=(), *, accent=BLUE, fill="#ffffff", size=21):
        self.rect(x, y, width, height, fill)
        self.rect(x + 1, y + 14, 3, height - 28, accent, "none", 1)
        lines = [title, *body]
        baseline = y + (height - (len(lines) - 1) * 26) / 2 + 7
        self.text(x + 20, baseline, title, size, INK, 650)
        for i, line in enumerate(body, 1):
            self.text(x + 20, baseline + 26 * i, line, 18, MUTED)

    def arrow(self, points, *, color="blue", dashed=False, both=False):
        d = "M" + " L".join(f"{x} {y}" for x, y in points)
        attrs = {"d": d, "fill": "none", "stroke": {"blue": BLUE, "teal": TEAL, "muted": MUTED}[color],
                 "stroke_width": 2, "stroke_linejoin": "round", "marker_end": f"url(#{color})"}
        if both:
            attrs["marker_start"] = f"url(#{color})"
        if dashed:
            attrs["stroke_dasharray"] = "6 5"
        el(self.canvas, "path", **attrs)

    def output(self) -> str:
        ET.indent(self.root, space="  ")
        return ET.tostring(self.root, encoding="unicode") + "\n"


def architecture(en: bool) -> str:
    def tr(zh, english):
        return english if en else zh

    d = Diagram(878, tr("同一个核心，三种运行形态", "One Runtime core. Three ways to run."),
                tr("Echo Agent Runtime 提供核心能力；终端、Gateway 与 Client 按各自场景承载运行。", "Echo Agent Runtime provides the core; Terminal, Gateway, and Client host it for different needs."),
                tr("统一的 Echo Agent Runtime 核心支撑三种形态：终端本地运行、Gateway 服务、Client 进程内嵌入。核心职责包括 Agent Loop、上下文与记忆、模型路由、工具与 MCP、Skills 与进化、权限与可观测性。模型和外部工具按配置访问，状态由当前实例和宿主管理。图表达逻辑架构，不表示三种形态共享同一个进程或同一份状态。", "The Echo Agent Runtime core supports local terminal execution, Gateway service, and in-process Client embedding. Core responsibilities include the Agent Loop, context and memory, model routing, tools and MCP, skills and evolution, permissions and observability. External services follow configuration; each host and instance owns its state. This is a logical architecture, not a shared-process or shared-state topology."))
    hosts = [
        (36, tr("本地运行 / Terminal", "Local / Terminal"), "echo-agent run", tr("启动 Runtime 实例", "Start a Runtime instance"), BLUE),
        (420, tr("在线服务 / Gateway", "Service / Gateway"), "Dashboard / API / Channels", tr("服务中运行 Runtime", "Run Runtime as a service"), BLUE),
        (804, tr("桌面应用 / Client", "Desktop / Client"), "Workspace / IDE / Automation", tr("进程内嵌入 Runtime", "Embed Runtime in process"), TEAL),
    ]
    for x, title, body, embedded, color in hosts:
        d.rect(x, 132, 360, 146)
        d.text(x + 20, 169, title, 23, INK, 700)
        d.text(x + 20, 200, body, 18, MUTED)
        d.rect(x + 20, 221, 320, 36, "#edf4ff" if color == BLUE else "#edf9f7", "none", 8)
        d.text(x + 180, 245, embedded, 18, color, 600, anchor="middle")
        d.arrow([(x + 180, 278), (x + 180, 340)], color="teal" if color == TEAL else "blue")
    d.rect(36, 340, 1128, 320)
    d.text(60, 383, "Echo Agent Runtime", 29, INK, 700)
    d.text(60, 413, tr("统一的核心职责 · 具体能力按宿主和配置启用", "Core responsibilities · features follow host integration and configuration"), 18, MUTED)
    capabilities = [
        (60, 438, "Agent Loop", tr("任务规划 · 推理 · 执行反馈", "Plans / tasks / execution feedback")),
        (428, 438, tr("上下文与记忆", "Context and memory"), "Sessions / Memory / Knowledge"),
        (796, 438, tr("模型接入与路由", "Models and routing"), "Providers / routing / fallback"),
        (60, 542, tr("工具与 MCP", "Tools and MCP"), tr("工具调用 · 连接器 · 执行后端", "Tools / MCP / executors")),
        (428, 542, tr("Skills 与进化", "Skills and evolution"), tr("技能复用 · 候选生成 · 评估", "Skill reuse / candidates / eval")),
        (796, 542, tr("权限与可观测性", "Policy and observability"), tr("审批 · 日志 · 用量与状态", "Approval / logs / usage")),
    ]
    for x, y, title, body in capabilities:
        d.node(x, y, 344, 78, title, (body,), accent=TEAL if y == 542 else BLUE, size=20)
    d.node(36, 728, 548, 80, tr("所配置的模型与外部工具服务", "Configured models and external tools"),
           (tr("本地或远端模型 · MCP / 外部 API", "Local or remote models · MCP / external APIs"),), size=21)
    d.node(616, 728, 548, 80, tr("实例与宿主的本地状态", "Local instance and host state"),
           (tr("会话 · 记忆 · 技能 · 工作区文件", "Sessions · memory · skills · workspace files"),), accent=TEAL, size=21)
    d.arrow([(310, 660), (310, 728)], both=True)
    d.arrow([(890, 660), (890, 728)], color="teal", dashed=True, both=True)
    d.text(600, 850, tr("上层表示承载关系，下层表示服务与数据依赖；各运行实例的进程和状态由其宿主管理。", "Top: hosting modes. Bottom: service and data dependencies. Each host owns its processes and state."), 17, MUTED, anchor="middle")
    return d.output()


def runtime(en: bool) -> str:
    def tr(zh, english):
        return english if en else zh

    d = Diagram(870, tr("Runtime：从入口到执行", "Runtime: from input to execution"),
                tr("app.py 装配生命周期；Gateway 为可选服务入口，AgentLoop 负责执行任务。", "app.py owns lifecycle composition; Gateway is optional; AgentLoop executes tasks."),
                tr("消息通道和本地终端通过 ChannelManager 连接 MessageBus；Dashboard、CLI/TUI 和应用通过 HTTP/WebSocket 连接可选 Gateway，再进入 MessageBus。AgentLoop 消费入站事件并发布出站事件。循环使用 ContextStage、InferenceStage 与 ResponseStage，依赖会话、记忆、知识、模型、工具和审批模块。SQLite 与文件保存状态；执行轨迹在启用进化后进入 EvolutionEngine。", "Channels and the local terminal connect via ChannelManager to MessageBus. Dashboard, CLI/TUI, and apps connect through HTTP/WebSocket to optional Gateway and then MessageBus. AgentLoop consumes inbound and publishes outbound events. Its pipeline uses ContextStage, InferenceStage, and ResponseStage, with sessions, memory, knowledge, models, tools, and approval dependencies. SQLite/files persist state; evolution consumes execution trajectories when enabled."))
    d.node(36, 128, 500, 76, tr("终端 / 消息通道", "Terminal / messaging channels"), (tr("本地 CLI 通道 · IM · Webhook", "Local CLI channel · IM · Webhook"),))
    d.node(664, 128, 500, 76, tr("Dashboard / CLI·TUI / 你的应用", "Dashboard / CLI·TUI / your app"), ("HTTP / WebSocket",), accent=TEAL)
    d.node(36, 248, 500, 76, "ChannelManager", ("echo_agent/channels/",))
    d.node(664, 248, 500, 76, "GatewayServer", (tr("可选 · aiohttp · 认证与管理 API", "Optional · aiohttp · auth and management API"),), accent=TEAL)
    d.arrow([(286, 204), (286, 248)], both=True)
    d.arrow([(914, 204), (914, 248)], color="teal", both=True)
    d.node(324, 372, 552, 76, "MessageBus", ("InboundEvent / OutboundEvent",))
    d.arrow([(286, 324), (286, 350), (456, 350), (456, 372)], both=True)
    d.arrow([(914, 324), (914, 350), (744, 350), (744, 372)], color="teal", both=True)
    d.node(240, 494, 720, 90, "AgentLoop", ("ContextStage → InferenceStage → ResponseStage",), fill="#edf4ff")
    d.arrow([(600, 448), (600, 494)], both=True)
    boxes = [
        (36, tr("上下文与状态", "Context and state"), ("Sessions / Memory / Knowledge",), BLUE),
        (420, tr("模型与工具执行", "Models and tool execution"), ("Router / Tools / MCP / Approval",), BLUE),
        (804, tr("Skills 与进化", "Skills and evolution"), ("Skills / Plugins / Evolution",), TEAL),
    ]
    for x, title, body, color in boxes:
        d.node(x, 642, 360, 84, title, body, accent=color, size=20)
        d.arrow([(600, 584), (600, 616), (x + 180, 616), (x + 180, 642)],
                color="muted", dashed=True)
    d.rect(36, 770, 1128, 58, "#edf3f9")
    d.text(600, 806, tr("持久化：SQLite + 工作区文件；自动化、工具集与进化均按配置启用。", "Persistence: SQLite + workspace files. Automation, tools, and evolution follow configuration."), 18, MUTED, anchor="middle")
    return d.output()


def learning(en: bool) -> str:
    def tr(zh, english):
        return english if en else zh

    d = Diagram(850, tr("记忆延续上下文，进化沉淀技能", "Memory carries context. Evolution builds skills."),
                tr("两条链路共同服务后续任务；技能进化需显式启用，候选不直接写入正式技能库。", "Both support future tasks. Skill evolution is opt-in; proposals do not go straight to the live library."),
                tr("Runtime 记忆链路：Working 提供当前上下文；Episodic 和 Semantic 保存摘要与事实，经作用域和有效性筛选后混合召回，与当前上下文一起送入 ContextStage。Archival、已取代或未解决冲突的条目不参与正常召回。进化链路：任务轨迹和反思生成候选，经校验、风险检查和隔离 A/B 评估决定晋升、待审核或拒绝；已晋升技能可手动回滚。", "Runtime memory: Working supplies current context; Episodic and Semantic retain summaries and facts. Scope and lifecycle eligibility filter mixed recall, which joins current context in ContextStage. Archived, superseded, and unresolved entries are excluded from normal recall. Evolution: trajectories and reflection produce candidates; validation, risk checks, and isolated A/B evaluation decide promotion, review, or rejection. Promoted changes support manual rollback."))
    for x, label, color in ((36, tr("01  认知记忆", "01  Cognitive memory"), BLUE),
                             (616, tr("02  技能自进化", "02  Self-evolving skills"), TEAL)):
        d.rect(x, 128, 548, 620)
        d.text(x + 24, 172, label, 25, color, 700)
    d.node(76, 206, 468, 72, tr("当前任务与会话上下文", "Current task and session context"), ("Working",))
    d.node(76, 318, 468, 86, tr("会话摘要与长期事实", "Session summaries and durable facts"), ("Episodic / Semantic",))
    d.node(76, 454, 468, 96, tr("作用域 / 有效性筛选与混合召回", "Scope, eligibility, and hybrid recall"), ("BM25 + optional Vector / RRF",), size=20)
    d.arrow([(310, 404), (310, 454)])
    d.node(76, 604, 468, 72, "ContextStage", (tr("当前上下文 + 快照 + 相关记忆", "Current context + snapshots + recalled memory"),), fill="#edf4ff")
    d.arrow([(310, 550), (310, 604)])
    d.arrow([(76, 242), (56, 242), (56, 640), (76, 640)])
    d.text(76, 714, tr("Archival / 已取代 / 未解决冲突 → 正常召回排除", "Archived / superseded / unresolved → excluded"), 17, MUTED)
    d.node(640, 206, 500, 72, tr("任务轨迹与反思", "Execution trajectories and reflection"), ("Recorder / TrajectoryStore",), accent=TEAL)
    d.node(640, 318, 500, 86, tr("生成技能候选", "Propose skill candidates"), ("Evolver → SkillCandidate",), accent=TEAL)
    d.arrow([(890, 278), (890, 318)], color="teal")
    d.node(640, 454, 500, 96, tr("校验 / 风险检查 / 隔离 A/B 评估", "Validate / risk-check / isolated A/B eval"), ("Validation + PromotionGate",), accent=TEAL, size=20)
    d.arrow([(890, 404), (890, 454)], color="teal")
    d.node(640, 604, 236, 72, tr("通过 → 晋升技能", "Pass → promote"), (tr("满足晋升策略时", "When policy permits"),), accent=TEAL, fill="#edf9f7", size=19)
    d.node(902, 604, 238, 72, tr("待审核 / 拒绝", "Review / reject"), (tr("风险或配置决定", "Risk / policy / eval"),), accent=MUTED, size=19)
    d.arrow([(890, 550), (890, 578), (758, 578), (758, 604)], color="teal")
    d.arrow([(890, 578), (1020, 578), (1020, 604)], color="muted")
    d.text(640, 714, tr("晋升后用于后续任务；支持手动回滚。", "Promoted skills serve future tasks; rollback is manual."), 17, MUTED)
    d.text(600, 797, tr("记忆整合与进化在后台按配置运行；不代表模型权重训练或每次执行都自动提升。", "Background consolidation and evolution follow configuration; evolution updates skills, not model weights."), 18, MUTED, anchor="middle")
    return d.output()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Fail if generated SVGs differ from their sources")
    args = parser.parse_args()
    failed = []
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for name, build in (("architecture", architecture), ("runtime-architecture", runtime), ("memory-evolution", learning)):
        for en in (False, True):
            path = OUTPUT / f"{name}{'.en' if en else ''}.svg"
            content = build(en)
            if args.check:
                if not path.is_file() or path.read_text(encoding="utf-8") != content:
                    failed.append(str(path.relative_to(ROOT)))
            else:
                path.write_text(content, encoding="utf-8")
                print(path.relative_to(ROOT))
    if failed:
        parser.exit(1, "Regenerate diagrams: " + ", ".join(failed) + "\n")
    if args.check:
        print("All six README diagrams match their source.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
