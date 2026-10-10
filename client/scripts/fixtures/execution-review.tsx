/**
 * Isolated Agent execution review. The transcript and lifecycle events below
 * are fixture data, rendered by production ChatView/ExecutionProcess/tool and
 * runtime components. They do not run an Agent, shell, filesystem or service.
 * The host must install its existing in-memory Tauri invoke mock before mount.
 */
import { useLayoutEffect, useRef, useState } from "react";
import { ChatView } from "../../src/components/ChatView";
import { SubagentPanel } from "../../src/components/SubagentPanel";
import { TeamStatusView } from "../../src/components/TeamStatusView";
import { useSessionStore, type ToolCallView } from "../../src/stores/session-store";
import { useSessionsStore } from "../../src/stores/sessions-store";
import { useSubagentStore } from "../../src/stores/subagent-store";
import type { RuntimeTeamInfo } from "../../src/lib/agent-client";
import type { SessionControlAction } from "../../src/lib/session-control";

export type ExecutionReviewState = "running" | "complete" | "error" | "empty";
export const executionReviewSessionId = "review-isolated-execution";
export const executionReviewCwd = "/review/isolated-EchoAgent";
const promptId = "review-isolated-prompt";
const reportPath = `${executionReviewCwd}/generated/界面评审与回归验证报告.md`;
const longPath = `${executionReviewCwd}/generated/${"delivery_notes_for_customer_release_".repeat(7)}.md`;
const toolIds = {
  team: "execution-team", visual: "execution-visual-agent", result: "execution-result-agent",
  read: "execution-read", terminal: "execution-terminal", edit: "execution-edit",
};

export function readExecutionReviewState(): ExecutionReviewState {
  const state = typeof location === "undefined" ? null : new URLSearchParams(location.search).get("state");
  return state === "complete" || state === "error" || state === "empty" ? state : "running";
}

let currentReviewState: ExecutionReviewState = readExecutionReviewState();
let streamSequence = 0;

const finalTable = [
  "| 检查项 | 正常 | 空数据 | 加载 | 错误 | 长内容 | 小窗口 | 深色主题 | 操作反馈 | 结论 |",
  "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
  ...Array.from({ length: 12 }, (_, index) => `| 隔离验证 ${index + 1} | 可读 | 有说明 | 有状态 | 可恢复 | 无溢出 | 1024 × 680 | 清晰 | 可定位 | 验证数据 |`),
].join("\n");

export const executionReviewFinalMarkdown = `# 隔离验证：界面评审交付\n\n这份答复用于检查最终结果与中间执行过程的分组、排版和滚动。它不是模型执行或真实服务成功的证据。\n\n## 主要结果\n\n- 弹窗按内容组织标题、说明、字段与操作。\n- 生成文件可以从任务成果入口预览；本场景读取的是内存中的隔离文档。\n- 工具调用和两名子代理的执行记录保留在上方执行过程中。\n\n## 覆盖矩阵\n\n${finalTable}\n\n## 示例实现\n\n\`\`\`typescript\nconst isolatedArtifact = "${longPath}";\nexport async function previewDelivery() {\n  return { path: isolatedArtifact, source: "ui-review-fixture" };\n}\n\`\`\`\n\n## 详细说明\n\n${Array.from({ length: 18 }, (_, index) => `${index + 1}. 这是可复现的隔离结果段落，用于检查持续增长的长文本、用户主动向上阅读和回到最新消息。结论、验证矩阵与产物入口应保持清晰，不应被中间日志淹没。`).join("\n\n")}\n\n**隔离文档路径：** \`${reportPath}\`。`;

const commandOutput = Array.from({ length: 150 }, (_, index) =>
  `[isolated ${String(index + 1).padStart(3, "0")}] ${index === 1 ? longPath : "检查布局、执行状态与产物入口；此行是内存中的验证日志。"}`,
).join("\n");

type Update = Parameters<ReturnType<typeof useSessionStore.getState>["applyUpdate"]>[0];

function update(sessionUpdate: string, fields: Record<string, unknown>) {
  // ACP payloads use sessionUpdate and flatten tool deltas at the top level,
  // matching session-store's wire-ingestion regression fixtures.
  useSessionStore.getState().applyUpdate({
    sessionUpdate, ...fields, __sessionId: executionReviewSessionId,
  } as unknown as Update);
}

function tool(toolCallId: string, kind: string, title: string, rawInput: unknown, content: ToolCallView["content"], status: ToolCallView["status"] = "completed") {
  update("tool_call", { toolCallId, kind, title, rawInput, content, status });
}

function updateTool(toolCallId: string, status: ToolCallView["status"], content?: ToolCallView["content"]) {
  update("tool_call_update", { toolCallId, status, ...(content ? { content } : {}) });
}

function seedSubagents() {
  const store = useSubagentStore.getState();
  const agents = [
    { id: "review-visual-child", description: "检查弹窗比例、导航与长内容布局", subagentType: "explore" },
    { id: "review-result-child", description: "核验工具结果、产物预览与复制反馈", subagentType: "general-purpose" },
  ];
  for (const [index, agent] of agents.entries()) {
    store.applyEvent({
      sessionId: executionReviewSessionId, phase: "spawned", subagentId: agent.id,
      childSessionId: agent.id, parentPromptId: promptId, description: agent.description,
      subagentType: agent.subagentType, model: "review/isolated-model", status: "running",
      occurredAt: Date.now() - 8_000,
    });
    store.applyEvent({
      sessionId: executionReviewSessionId, phase: "progress", subagentId: agent.id,
      durationMs: 8_000 + index * 2_000, turnCount: 2 + index, toolCallCount: 4 + index,
      tokensUsed: 6_400 + index * 800, contextWindowTokens: 128_000,
      contextUsagePct: 5 + index, toolsUsed: ["read_file", "run_terminal_command"], errorCount: 0,
    });
  }
}

function finishSubagents(state: "complete" | "error") {
  const store = useSubagentStore.getState();
  for (const [index, subagentId] of ["review-visual-child", "review-result-child"].entries()) {
    const failed = state === "error" && index === 1;
    store.applyEvent({
      sessionId: executionReviewSessionId, phase: "finished", subagentId,
      status: failed ? "failed" : "completed", durationMs: 16_000 + index * 3_000,
      turnCount: 3 + index, toolCallCount: 7 + index, errorCount: failed ? 1 : 0,
      output: failed ? "已保留可读的部分产物和检查记录。" : "隔离检查记录：已核验正常、长内容和窄窗口布局；真实服务链路仍需另行验证。\n".repeat(8),
      error: failed ? "隔离失败：外部预览服务未配置，未发出真实请求。" : undefined,
    });
  }
}

export function seedExecutionReview(state: ExecutionReviewState = readExecutionReviewState(), userText?: string) {
  currentReviewState = state;
  streamSequence = 0;
  const chat = useSessionStore.getState();
  chat.dropSessionCache(executionReviewSessionId);
  chat.setSession(executionReviewSessionId);
  chat.setError(null);
  useSubagentStore.getState().clearSession(executionReviewSessionId);
  const catalog = useSessionsStore.getState();
  catalog.setCurrent(executionReviewSessionId);
  catalog.clearDraft(executionReviewSessionId);
  catalog.setIndependent([{
    sessionId: executionReviewSessionId, title: "隔离验证 · Agent 执行与产物交付",
    cwd: executionReviewCwd, status: state === "running" ? "working" : state === "error" ? "failed" : state === "empty" ? "pending" : "completed",
  }]);
  if (state === "empty") return;

  chat.pushUser(userText ?? "请检查 UI、执行过程与生成结果，安排两名子代理并行检查，汇总可阅读的最终报告。\n这是隔离界面数据，不调用真实模型、工具或外部服务。", [], executionReviewSessionId);
  chat.startStreaming(executionReviewSessionId, promptId);
  update("agent_message_chunk", { content: { text: "我会先建立界面与交互基线，再并行检查视觉布局和结果交付。以下过程是隔离验证内容。" } });
  update("agent_thought_chunk", { content: { text: "需要把最终结论与中间日志分开。检查小窗口下的标题、折叠入口、工具摘要与子代理进度；当用户向上阅读时，应保留位置并提供回到最新消息入口。\n".repeat(12) } });
  tool(toolIds.team, "echoagent__create_team", "创建隔离 UI 评审团队", {
    team_id: "review-isolated-team", members: ["visual-reviewer", "result-reviewer"],
  }, [{ type: "text", text: "隔离快照：review-isolated-team，成员 visual-reviewer、result-reviewer。" }]);
  tool(toolIds.visual, "spawn_subagent", "检查弹窗比例、导航与长内容布局", {
    task_id: "review-visual-child", description: "检查弹窗比例、导航与长内容布局", subagent_type: "explore",
    prompt: `检查对话、弹窗与结果预览在最小窗口和两种主题下的比例、对齐、层级与滚动。长产物路径：${longPath}。以上均为隔离数据。`,
  }, [{ type: "text", text: "Subagent moved to the background and is still running.\nsubagent_id: review-visual-child\ntype: explore" }], "in_progress");
  tool(toolIds.result, "spawn_subagent", "核验工具结果、产物预览与复制反馈", {
    task_id: "review-result-child", description: "核验工具结果、产物预览与复制反馈", subagent_type: "general-purpose",
    prompt: "核验长日志、Markdown 宽表格、代码块、失败提示与文件操作反馈。保留部分失败的结果。此任务仅为隔离 UI 数据。",
  }, [{ type: "text", text: "Subagent moved to the background and is still running.\nsubagent_id: review-result-child\ntype: general-purpose" }], "in_progress");
  seedSubagents();
  tool(toolIds.read, "read_file", `Read ${longPath}`, { path: longPath, max_lines: 200 }, [
    { type: "text", text: `# 隔离读取结果\n\n${"用于核验长工具结果的层级与滚动。这段数据存储在内存中，不对应真实文件读取。\n\n".repeat(40)}` },
  ]);
  tool(toolIds.terminal, "run_terminal_command", "Run pnpm test -- --isolated-ui-fixture", {
    command: "pnpm test -- --isolated-ui-fixture", cwd: executionReviewCwd,
    note: "此命令仅是工具参数展示，不会执行。",
  }, [{ type: "command_output", command: "pnpm test -- --isolated-ui-fixture", output: commandOutput, exitCode: state === "running" ? null : state === "error" ? 1 : 0 }], state === "running" ? "in_progress" : state === "error" ? "failed" : "completed");
  tool(toolIds.edit, "edit", `Write ${reportPath}`, { path: reportPath, note: "隔离内存文档" }, [
    { type: "diff", diff: { path: reportPath, old: "# 待整理\n", new: executionReviewFinalMarkdown } },
  ]);
  chat.setPlan({ entries: [
    { content: "建立界面与执行状态检查基线", priority: "high", status: "completed" },
    { content: "并行核验视觉布局与结果交付", priority: "high", status: state === "running" ? "in_progress" : "completed" },
    { content: "汇总结果并验证产物操作", priority: "medium", status: state === "running" ? "pending" : "completed" },
  ] });
  if (state !== "running") finishExecutionReview(state);
}

export function appendExecutionReviewChunk() {
  if (!useSessionStore.getState().streaming) return false;
  streamSequence += 1;
  update("agent_thought_chunk", { content: { text: `\n隔离流式更新 ${streamSequence}：继续核验执行状态与阅读位置。${"内容持续增长时，折叠入口、工具摘要和结果交付应保持稳定。".repeat(10)}\n` } });
  updateTool(toolIds.terminal, "in_progress", [{
    type: "command_output", command: "pnpm test -- --isolated-ui-fixture",
    output: `${commandOutput}\n${Array.from({ length: streamSequence * 6 }, (_, index) => `[追加 ${index + 1}] 隔离日志：等待并行检查完成。`).join("\n")}`, exitCode: null,
  }]);
  return true;
}

export function finishExecutionReview(state: "complete" | "error" = "complete") {
  if (!useSessionStore.getState().streaming) return false;
  currentReviewState = state;
  updateTool(toolIds.visual, "completed", [{ type: "text", text: "隔离视觉检查完成。\nsubagent_id: review-visual-child\ntype: explore" }]);
  updateTool(toolIds.result, state === "error" ? "failed" : "completed", [{ type: "text", text: state === "error" ? "隔离失败：预览服务未配置；已保留部分结果。\nsubagent_id: review-result-child" : "隔离结果展示检查完成。\nsubagent_id: review-result-child" }]);
  updateTool(toolIds.terminal, state === "error" ? "failed" : "completed", [{
    type: "command_output", command: "pnpm test -- --isolated-ui-fixture",
    output: `${commandOutput}\n${state === "error" ? "隔离失败：1 个未配置服务场景，未发出真实请求。" : "隔离日志结束。此内容不代表真实测试已通过。"}`, exitCode: state === "error" ? 1 : 0,
  }]);
  finishSubagents(state);
  update("agent_message_chunk", { content: { text: state === "error" ? `# 隔离验证：部分结果已保留\n\n工具预览因未配置外部服务而失败，过程与可读产物仍可查看。请打开失败工具和子代理记录了解详情。\n\n${executionReviewFinalMarkdown}` : executionReviewFinalMarkdown } });
  useSessionStore.getState().setPlan({ entries: [
    { content: "建立界面与执行状态检查基线", priority: "high", status: "completed" },
    { content: "并行核验视觉布局与结果交付", priority: "high", status: "completed" },
    { content: state === "error" ? "已保留部分结果，服务配置仍待完成" : "汇总隔离验证结果", priority: "medium", status: state === "error" ? "pending" : "completed" },
  ] });
  useSessionStore.getState().markComplete({
    sessionId: executionReviewSessionId, promptId,
    stopReason: state === "error" ? "error" : "end_turn",
    agentResult: state === "error" ? "隔离失败：外部预览服务未配置，未发出真实请求。" : undefined,
    usage: { promptTokens: 7_200, completionTokens: 3_600, totalTokens: 10_800 },
  });
  if (state === "error") useSessionStore.getState().setError("隔离失败：外部预览服务未配置；部分结果已保留，未发出真实请求。");
  return true;
}

/** Host invoke case: team_snapshot -> executionReviewTeamSnapshot(). */
export function executionReviewTeamSnapshot(): RuntimeTeamInfo[] {
  if (currentReviewState === "empty") return [];
  if (new URLSearchParams(location.search).get("teamState") === "error") {
    throw new Error("隔离快照读取失败；未请求真实团队服务。");
  }
  return [{ teamId: "review-isolated-team", members: ["visual-reviewer", "result-reviewer"], createdAt: 1_791_581_400_000 }];
}

export default function ExecutionReviewFixture() {
  const initial = useRef(readExecutionReviewState());
  const [ready, setReady] = useState(false);
  const [feedback, setFeedback] = useState("");
  const streaming = useSessionStore((store) => store.streaming);
  const messages = useSessionStore((store) => store.messages);
  const viewQuery = new URLSearchParams(location.search);
  const runtimeOnly = viewQuery.get("executionView") === "runtime" || viewQuery.get("surface") === "runtime";
  useLayoutEffect(() => {
    seedExecutionReview(initial.current);
    setReady(true);
  }, []);

  const cancel = (action: SessionControlAction = "stop") => {
    const chat = useSessionStore.getState();
    chat.requestControl(executionReviewSessionId, action, promptId);
    chat.confirmControl(executionReviewSessionId, action);
    for (const runtime of useSubagentStore.getState().getForSession(executionReviewSessionId)) {
      if (runtime.status === "running") useSubagentStore.getState().applyEvent({ sessionId: executionReviewSessionId, phase: "finished", subagentId: runtime.id, status: "cancelled", output: "隔离本地状态已停止；未调用真实 Runtime。" });
    }
    setFeedback("隔离本地执行状态已停止；未调用真实 Runtime。");
    return true;
  };

  return <div className="execution-review-fixture" data-execution-ready={ready} style={{ display: "flex", flexDirection: "column", minWidth: 0, height: "100%", background: "var(--echo-bg-primary)" }}>
    <div style={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 8, padding: "8px 12px", borderBottom: "1px solid var(--echo-border-default)", fontSize: 12 }}>
      <strong>隔离验证 · Agent 执行过程</strong><span style={{ color: "var(--echo-text-medium)", flex: 1 }}>内存数据 · 无真实模型、命令或服务</span>
      <button type="button" className="btn btn--secondary" disabled={!streaming} onClick={() => { appendExecutionReviewChunk(); setFeedback("已追加隔离流式内容"); }}>追加流式内容</button>
      <button type="button" className="btn btn--secondary" disabled={!streaming} onClick={() => { finishExecutionReview(); setFeedback("隔离任务已完成"); }}>完成当前任务</button>
      <button type="button" className="btn btn--secondary" disabled={!streaming} onClick={() => { finishExecutionReview("error"); setFeedback("已切换为隔离部分失败状态"); }}>模拟工具失败</button>
      <button type="button" className="btn btn--secondary" onClick={() => { seedExecutionReview("empty"); setFeedback("隔离会话已清空"); }}>清空会话</button>
    </div>
    <div role="status" style={{ padding: "6px 12px", minHeight: 28, boxSizing: "border-box", color: "var(--echo-text-medium)", fontSize: 12 }}>{feedback || "状态由隔离事件驱动；可检查流式更新、完成和部分失败。"}</div>
    {ready && (runtimeOnly ? <div className="chatview" style={{ minHeight: 0, flex: 1 }}><div className="chatview__scroll-shell"><div className="chatview__scroll"><div className="chatview__inner">
      <SubagentPanel messages={messages} cwd={executionReviewCwd} onOpenSession={() => { setFeedback("隔离子代理导航入口已触发；未加载真实工作记录。"); }} />
      <TeamStatusView messages={messages} />
    </div></div></div></div> : <ChatView
      onSend={(text) => { seedExecutionReview("running", text); setFeedback("隔离发送已接收；未请求真实模型。"); return true; }}
      onCancel={cancel} onToast={setFeedback}
      onOpenSubagentSession={() => { setFeedback("隔离子代理导航入口已触发；未加载真实工作记录。"); }}
      cwd={executionReviewCwd} models={[{ id: "review/isolated-model", label: "隔离验证模型", providerId: "review", providerKind: "custom" }]}
      modelId="review/isolated-model" title="隔离验证 · 执行过程与最终交付"
    />)}
  </div>;
}
