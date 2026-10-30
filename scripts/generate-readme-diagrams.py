#!/usr/bin/env python3
"""Build bilingual SVG diagrams for the README and documentation site.

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
    # SVG coordinates need no sub-millipixel precision. Normalize floats so
    # equivalent geometry has identical output across Python versions.
    def value(item: object) -> str:
        if isinstance(item, float):
            return f"{item:.3f}".rstrip("0").rstrip(".")
        return str(item)

    return ET.SubElement(parent, f"{{{NS}}}{tag}", {k.replace("_", "-"): value(v) for k, v in attrs.items()})


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



def text_units(content: str) -> float:
    # Integer accumulation avoids the version-dependent float sum algorithm.
    return sum(5 if ord(char) < 128 else 9 for char in content) / 5


def label(d, x, y, content, size=17):
    """Relation labels have an opaque background so routed lines remain legible."""
    width = text_units(content) * size * 0.53 + 18
    d.rect(x - width / 2, y - size, width, size + 7, "#f7faff", "none", 2)
    d.text(x, y, content, size, MUTED, anchor="middle")


def boundary(d, x, y, w, h, name):
    node = d.rect(x, y, w, h, "none", MUTED, 4)
    node.set("stroke-dasharray", "7 5")
    label(d, x + 20 + text_units(name) * 4.8, y + 29, name, 18)


def architecture(en: bool) -> str:
    def tr(zh, english):
        return english if en else zh
    d = Diagram(1050, tr("C4 系统上下文：Desktop 与 Runtime", "C4 system context: Desktop and Runtime"),
                tr("读图范围：两个产品、用户及外部服务；不展开内部组件。", "Scope: two products, their user, and external services; internal components are omitted."),
                tr("用户分别使用 Desktop 与 Runtime。Desktop 内嵌 Rust 引擎，Runtime 使用 Python 引擎；两者分别管理状态，均可调用模型和外部工具。当前没有 Desktop 到 Python Gateway 的默认执行连线。", "A user accesses Desktop and standalone Runtime independently. Desktop embeds a Rust engine; Runtime uses Python. Each owns its state and can call models and external tools. No default Desktop-to-Python-Gateway execution link is implied."))
    d.node(400, 128, 400, 90, tr("用户", "User"), ("[Person]",), size=24)
    boundary(d, 36, 300, 548, 310, tr("本机交互工作", "Interactive local work"))
    boundary(d, 616, 300, 548, 310, tr("终端或自托管部署", "Terminal or self-hosted deployment"))
    d.node(66, 375, 488, 116, "EchoAgent Desktop", ("[Software System · Tauri / Rust]", tr("内嵌执行引擎、工作区与任务界面", "Embedded engine, workspace, and task UI")), accent=TEAL)
    d.node(646, 375, 488, 116, "Echo Agent Runtime", ("[Software System · Python]", tr("独立执行引擎、通道与可选 Gateway", "Standalone engine, channels, optional Gateway")))
    label(d, 310, 548, tr("独立模型配置、会话、记忆与本地状态", "Own model config, sessions, memory, state"), 18)
    label(d, 890, 548, tr("独立模型配置、会话、记忆与服务状态", "Own model config, sessions, memory, state"), 18)
    d.arrow([(480, 218), (480, 260), (310, 260), (310, 375)])
    label(d, 290, 280, tr("通过桌面界面提交任务", "Submits tasks via desktop UI"))
    d.arrow([(720, 218), (720, 260), (890, 260), (890, 375)])
    label(d, 900, 280, tr("终端 / HTTP / WebSocket", "Terminal / HTTP / WebSocket"))
    d.node(66, 725, 488, 106, tr("模型服务", "Model service"), ("[External Software System]", tr("本地或远端推理、嵌入与重排", "Local or remote inference and retrieval")), accent=MUTED)
    d.node(646, 725, 488, 106, tr("工具与业务服务", "Tool and business services"), ("[External Software System]", "MCP / external APIs"), accent=MUTED)
    d.arrow([(190, 491), (190, 725)])
    label(d, 190, 666, tr("请求模型处理", "Requests model processing"))
    d.arrow([(770, 491), (770, 635), (430, 635), (430, 725)])
    label(d, 490, 633, tr("请求模型处理", "Requests model processing"))
    d.arrow([(66, 450), (20, 450), (20, 880), (890, 880), (890, 831)], color="teal")
    label(d, 600, 902, tr("Desktop 调用已配置工具", "Desktop calls configured tools"))
    d.arrow([(1010, 491), (1148, 491), (1148, 693), (1010, 693), (1010, 725)])
    label(d, 1010, 692, tr("调用已配置工具", "Calls configured tools"))
    d.text(600, 978, tr("图例：方框内标注类型与职责；有向实线表示调用关系；虚线框表示使用或部署范围。", "Legend: boxes name type and responsibility; arrows show calls; dashed boxes group usage/deployment scope."), 17, MUTED, anchor="middle")
    d.text(600, 1015, tr("箭头表示请求方向，响应沿原交互返回；两端配置和状态不自动同步。", "Arrows show request direction; responses return through the interaction. Product state is not auto-synchronized."), 17, MUTED, anchor="middle")
    return d.output()


def runtime(en: bool) -> str:
    def tr(zh, english):
        return english if en else zh
    d = Diagram(990, tr("Runtime 组件视图", "Runtime component view"),
                tr("读图范围：Python Runtime；实线为请求或数据流，虚线为组件依赖。", "Scope: Python Runtime. Solid arrows carry requests/data; dashed arrows show dependencies."),
                tr("终端与消息平台通过通道接入，网页与 API 客户端通过 Gateway 接入，入站事件进入事件总线和执行循环。循环依赖上下文、模型工具及记忆技能组件，状态写入持久化存储。", "Terminal and messaging inputs use channels; browser and API clients use Gateway. Events enter the bus and execution loop, which depends on context, model/tool, and memory/skill components and persists state."))
    d.node(36, 126, 500, 72, tr("终端与消息平台 [外部实体]", "Terminal and messaging [External]"))
    d.node(664, 126, 500, 72, tr("网页与 API 客户端 [外部实体]", "Browser and API clients [External]"))
    boundary(d, 20, 243, 1160, 630, "Echo Agent Runtime [Python process]")
    d.node(56, 300, 460, 76, tr("通道适配 [组件]", "Channel adapters [Component]"), (tr("标准化输入、交付回复", "Normalize input; deliver replies"),))
    d.node(684, 300, 460, 76, tr("Gateway [可选组件]", "Gateway [Optional component]"), (tr("认证、HTTP / WebSocket、管理界面", "Auth, HTTP / WebSocket, Dashboard"),), accent=TEAL)
    d.arrow([(286, 198), (286, 300)], both=True)
    label(d, 286, 225, tr("消息与回复", "Messages and replies"))
    d.arrow([(914, 198), (914, 300)], color="teal", both=True)
    label(d, 914, 225, "HTTP / WebSocket")
    d.node(330, 426, 540, 68, tr("事件总线 [组件]", "Event bus [Component]"))
    d.arrow([(286, 376), (286, 460), (330, 460)], both=True)
    label(d, 210, 412, tr("入站与出站事件", "Inbound/outbound events"))
    d.arrow([(914, 376), (914, 460), (870, 460)], color="teal", both=True)
    label(d, 990, 412, tr("入站与出站事件", "Inbound/outbound events"))
    d.node(330, 550, 540, 80, tr("任务执行循环 [组件]", "Agent loop [Component]"), (tr("组织上下文 → 推理与工具 → 保存结果", "Context → model/tools → save results"),))
    d.arrow([(600, 494), (600, 550)])
    label(d, 600, 530, tr("待处理任务", "Pending task"))
    d.arrow([(330, 590), (270, 590), (270, 480), (330, 480)])
    label(d, 190, 530, tr("回复与进度事件", "Reply/progress events"), 16)
    boxes=[(56, tr("上下文 [组件]", "Context [Component]"), tr("会话与知识检索", "Sessions and knowledge")),
           (428, tr("模型与工具 [组件]", "Models/tools [Component]"), tr("推理、审批与执行", "Inference, approval, execution")),
           (800, tr("记忆与技能 [组件]", "Memory/skills [Component]"), tr("召回、整合与可选进化", "Recall, consolidation, opt-in evolution"))]
    for x,title,body in boxes:
        d.node(x, 706, 344, 90, title, (body,), size=19)
        d.arrow([(600,630),(600,667),(x+172,667),(x+172,706)],color="muted",dashed=True)
    label(d, 600, 674, tr("使用", "Uses"))
    d.node(330, 902, 540, 62, tr("持久化存储 [SQLite / 文件]", "Persistent storage [SQLite / files]"), size=20)
    d.arrow([(228,796),(228,933),(330,933)],color="muted")
    d.arrow([(972,796),(972,933),(870,933)],color="muted")
    label(d, 600, 853, tr("存储数据：会话、记忆、轨迹与技能状态", "Stored data: sessions, memory, traces, skill state"))
    return d.output()


def learning(en: bool) -> str:
    def tr(zh, english):
        return english if en else zh
    d = Diagram(1000, tr("Runtime 记忆与技能进化流程", "Runtime memory and skill-evolution flow"),
                tr("两个独立流程：记忆为任务提供事实，进化将经验形成技能候选。", "Separate processes: memory supplies facts; evolution proposes reusable skills."),
                tr("左侧：筛选有效记忆、混合检索、组织上下文。右侧：显式启用进化后生成候选，经校验和评估，分为晋升、待审核或拒绝。", "Left: filter eligible memory, retrieve, and assemble context. Right: opt-in evolution proposes candidates, validates and evaluates them, then promotes, holds for review, or rejects."))
    for x,title in [(36,tr("记忆召回", "Memory recall")),(616,tr("技能进化（默认关闭）", "Skill evolution (off by default)"))]:
        boundary(d,x,120,548,800,title)
    def process(x,y,title):
        d.rect(x,y,448,64,"#fff",LINE,0)
        d.text(x+224,y+40,title,20,INK,650,anchor="middle")
    def terminator(x,y,title,w=280):
        d.rect(x,y,w,48,"#edf4ff",LINE,24)
        d.text(x+w/2,y+31,title,19,INK,600,anchor="middle")
    def decision(cx,cy,title):
        el(d.canvas,"polygon",points=f"{cx},{cy-60} {cx+180},{cy} {cx},{cy+60} {cx-180},{cy}",fill="#fff",stroke=LINE,stroke_width=1.5)
        d.text(cx,cy+6,title,19,INK,600,anchor="middle")
    terminator(170,180,tr("任务需要上下文", "Task needs context"))
    for y,title in [(290,tr("筛选作用域与有效性", "Filter scope and eligibility")),(440,tr("关键词与可选向量召回", "Keyword and optional vector recall")),(590,tr("融合排序与可选精排", "Fuse ranks; optionally rerank")),(740,tr("组合历史、知识与记忆", "Combine history, knowledge, memory"))]:
        process(86,y,title)
    for a,b in [(228,290),(354,440),(504,590),(654,740),(804,850)]:
        d.arrow([(310,a),(310,b)])
    terminator(170,850,tr("提供本轮上下文", "Supply turn context"))
    terminator(750,180,tr("已启用进化", "Evolution enabled"))
    process(666,280,tr("从轨迹与反思生成候选", "Propose from traces and reflection"))
    d.arrow([(890,228),(890,280)])
    process(666,385,tr("格式、风险与隔离评估", "Validate, risk-check, evaluate"))
    d.arrow([(890,344),(890,385)])
    decision(890,550,tr("评估与准入结论？", "Admission outcome?"))
    d.arrow([(890,449),(890,490)])
    for x,title in [(640,tr("晋升", "Promote")),(812,tr("待审核", "Review")),(984,tr("拒绝", "Reject"))]:
        d.rect(x,720,156,64,"#fff",LINE,4)
        d.text(x+78,759,title,20,INK,600,anchor="middle")
    d.arrow([(710,550),(650,550),(650,680),(718,680),(718,720)])
    label(d,718,672,tr("允许应用", "Admit"),16)
    d.arrow([(890,610),(890,720)])
    label(d,890,671,tr("需要确认", "Hold"),16)
    d.arrow([(1070,550),(1130,550),(1130,680),(1062,680),(1062,720)])
    label(d,1062,672,tr("未通过", "Fail"),16)
    terminator(750,850,tr("保存结果与原因", "Record result and reason"))
    for x in [718,890,1062]:
        d.arrow([(x,784),(x,818),(890,818),(890,850)],color="muted")
    d.text(600,963,tr("图例：圆角端点＝开始/结束，矩形＝处理，菱形＝判断；箭头为控制流。晋升后支持手动回滚。", "Legend: rounded terminals, processing rectangles, decision diamonds, control-flow arrows. Rollback is manual."),16,MUTED,anchor="middle")
    return d.output()


def data_flow(en: bool, desktop: bool = False) -> str:
    """Yourdon/DeMarco DFD: external rectangles, process circles, open stores."""
    def tr(zh, english):
        return english if en else zh
    name = "Desktop" if desktop else "Runtime"
    d=Diagram(980,tr(f"{name} 一级数据流图",f"{name} level-1 data-flow diagram"),
              tr("Yourdon / DeMarco 记法：数据流动，不表示调用顺序或控制判断。", "Yourdon / DeMarco notation: data movement, not execution order or control flow."),
              tr("用户输入进入任务处理，历史与记忆提供上下文，模型交换请求与结果，工具处理操作请求并返回结果，处理过程写入历史和交付物。", "User input enters task processing; history and memory supply context; model requests and responses are exchanged; tools return results; processing writes history and artifacts."))
    def external(x,y,title):
        d.rect(x,y,240,70,"#edf3f9",MUTED,0)
        d.text(x+120,y+43,title,20,INK,650,anchor="middle")
    def process(cx,cy,num,title):
        el(d.canvas,"circle",cx=cx,cy=cy,r=76,fill="#fff",stroke=BLUE,stroke_width=1.6)
        d.text(cx,cy-12,num,18,MUTED,anchor="middle")
        d.text(cx,cy+21,title,20,INK,650,anchor="middle")
    def store(x,y,title):
        for yy in [y,y+60]:
            el(d.canvas,"line",x1=x,y1=yy,x2=x+360,y2=yy,stroke=MUTED,stroke_width=2)
        d.text(x+180,y+38,title,20,INK,600,anchor="middle")
    external(40,170,tr("用户 / 接入客户端", "User / client"))
    external(900,170,tr("模型服务", "Model service"))
    external(900,600,tr("工具与外部系统", "Tools / external systems"))
    process(570,205,"1.0",tr("组织上下文", "Build context"))
    process(570,490,"2.0",tr("执行任务", "Execute task"))
    store(40,440,tr("D1 历史、记忆与知识", "D1 History, memory, knowledge"))
    store(390,825,tr("D2 执行记录与交付物", "D2 Execution records and artifacts"))
    d.arrow([(280,205),(494,205)])
    label(d,385,190,tr("目标与输入", "Goal and input"))
    d.arrow([(516,545),(470,545),(470,620),(220,620),(220,500)],color="teal")
    label(d,250,650,tr("新增历史与记忆", "History/memory updates"),16)
    d.arrow([(400,455),(445,455),(445,250),(510,250)])
    label(d,395,335,tr("相关内容", "Relevant content"))
    d.arrow([(570,281),(570,414)])
    label(d,570,355,tr("本轮上下文", "Turn context"))
    d.arrow([(646,465),(800,465),(800,205),(900,205)])
    label(d,770,190,tr("上下文与执行反馈", "Context and execution feedback"))
    d.arrow([(1020,240),(1020,490),(646,490)])
    label(d,835,515,tr("文本与工具调用请求", "Text and tool requests"),16)
    d.arrow([(630,537),(720,537),(720,625),(900,625)])
    label(d,790,609,tr("操作参数", "Operation parameters"))
    d.arrow([(900,655),(610,655),(610,555)],color="teal")
    label(d,780,680,tr("工具结果", "Tool results"))
    d.arrow([(570,566),(570,825)])
    label(d,570,755,tr("状态、结果与文件", "Status, results, files"))
    d.arrow([(516,435),(480,435),(480,365),(20,365),(20,145),(160,145),(160,170)],color="teal")
    label(d,190,350,tr("回复与执行状态", "Reply and execution status"),16)
    d.text(600,929,tr("图例：矩形＝外部实体；圆形＝处理过程；平行线＝数据存储；箭头及名称＝数据流。", "Legend: rectangle = external entity; circle = process; parallel lines = data store; named arrow = data flow."),17,MUTED,anchor="middle")
    d.text(600,961,tr("存储按所属实例或宿主管理；模型与外部工具可在本机或远端运行。", "Stores belong to the instance or host; models and external tools can run locally or remotely."),17,MUTED,anchor="middle")
    return d.output()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Fail if generated SVGs differ from their sources")
    args = parser.parse_args()
    failed = []
    OUTPUT.mkdir(parents=True, exist_ok=True)
    builders = (
        ("architecture", architecture),
        ("runtime-architecture", runtime),
        ("memory-evolution", learning),
        ("runtime-data-flow", data_flow),
        ("desktop-data-flow", lambda en: data_flow(en, desktop=True)),
    )
    for name, build in builders:
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
        print(f"All {len(builders) * 2} documentation diagrams match their source.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
