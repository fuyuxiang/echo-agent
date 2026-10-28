import { beforeEach, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { subscribeAgentEvents } from "../agent-client";
import { useSessionStore } from "@/stores/session-store";
import { MessageItem } from "@/components/MessageItem";
import { ThemeProvider } from "@/components/ThemeProvider";
import type { AcpSessionUpdate } from "../types";
import fixture from "./fixtures/acp-wire.json";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

beforeEach(() => {
  vi.resetAllMocks();
  localStorage.clear();
  useSessionStore.setState(useSessionStore.getInitialState());
  vi.mocked(invoke).mockResolvedValue({ permissions: [], questions: [], folderTrustRequests: [], planApprovals: [] });
});

it("由 Rust ACP 与 Tauri 序列化校验的事件经过真实客户端、Store 和消息 UI", async () => {
  const events = new Map<string, (payload: unknown) => void>();
  const unlisten = vi.fn();
  vi.mocked(listen).mockImplementation(async (name, handler) => {
    events.set(name, payload => handler({ payload } as never));
    return unlisten;
  });
  const store = useSessionStore.getState();
  store.setSession("wire-session");
  store.startStreaming("wire-session", "wire-prompt");
  store.setSession("foreground");
  const stop = await subscribeAgentEvents({ onUpdate: store.applyUpdate, onComplete: store.markComplete });
  try {
    for (const update of fixture.updates) events.get("agent://update")!(update);
    expect(useSessionStore.getState().messages).toEqual([]);
    const live = useSessionStore.getState().transcripts["wire-session"];
    const tool = live.messages.flatMap(m => m.parts).find(p => p.kind === "tool_call");
    expect(tool).toMatchObject({ kind: "tool_call", toolCall: {
      toolCallId: "wire-tool", kind: "other", status: "completed",
      rawInput: { path: "/工作区/文件 说明.md" },
      content: [{ type: "text", text: "读取结果" }, { type: "diff", diff: { path: "文件 说明.md", old: "", new: "新文件正文" } }],
    } });
    expect(live.plan?.entries).toEqual([{ content: "核验中文文件", priority: "high", status: "completed" }]);
    events.get("agent://complete")!(fixture.complete);
    const completed = useSessionStore.getState().transcripts["wire-session"];
    expect(completed.streamingMessageId).toBeNull();
    render(<ThemeProvider><MessageItem message={completed.messages[0]} streaming={false} /></ThemeProvider>);
    expect(screen.getByText("完成中文分析")).toBeVisible();
    store.startStreaming("wire-session", "failed-prompt");
    events.get("agent://complete")!(fixture.failure);
    expect(useSessionStore.getState().transcripts["wire-session"].messages.slice(-1)[0]).toMatchObject({
      complete: true, stopReason: "cancelled", cancellationCategory: "HookDenied", agentResult: "策略拒绝",
    });
    expect(useSessionStore.getState().sessionId).toBe("foreground");
  } finally { stop(); }
  expect(unlisten).toHaveBeenCalledTimes(2);
});

it("当前 ACP 类型拒绝旧数组 chunk 和嵌套 tool delta，旧 type 输入仍显式兼容", () => {
  const wire: AcpSessionUpdate = { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "当前格式" } };
  // @ts-expect-error ACP ContentChunk contains one block, never a block array.
  const invalidChunk: AcpSessionUpdate = { sessionUpdate: "agent_message_chunk", content: [{ type: "text", text: "旧数组" }] };
  // @ts-expect-error ACP ToolCallUpdateFields are flattened, never nested in update.
  const invalidDelta: AcpSessionUpdate = { sessionUpdate: "tool_call_update", toolCallId: "t", update: { status: "completed" } };
  void invalidChunk; void invalidDelta;
  const store = useSessionStore.getState();
  store.setSession("compat");
  store.applyUpdate(wire);
  store.applyUpdate({ type: "agent_message_chunk", content: [{ type: "text", text: "旧数据" }] });
  expect(useSessionStore.getState().messages[0].parts).toEqual([{ kind: "text", text: "当前格式旧数据" }]);
});
