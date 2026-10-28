import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { subscribeAgentEvents } from "../agent-client";
import { useSessionStore } from "@/stores/session-store";

beforeEach(() => {
  vi.resetAllMocks();
  useSessionStore.setState(useSessionStore.getInitialState());
  vi.mocked(invoke).mockResolvedValue({ permissions: [], questions: [], folderTrustRequests: [], planApprovals: [] });
});

it("挂起快照等待期间已关闭的计划审批不会被旧快照复活", async () => {
  const listeners = new Map<string, (payload: unknown) => void>();
  const unlisten = vi.fn();
  vi.mocked(listen).mockImplementation(async (event, handler) => {
    listeners.set(event, payload => handler({ payload } as never));
    return unlisten;
  });
  let releaseSnapshot!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementation(() => new Promise(resolve => { releaseSnapshot = resolve; }));
  const store = useSessionStore.getState();
  store.setSession("plan-session");
  const subscribing = subscribeAgentEvents({ onUpdate: update => store.applyUpdate(update) });
  await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
  listeners.get("agent://update")?.({ sessionId: "plan-session", sessionUpdate: "plan_approval_resolved", requestId: "closed-plan" });
  releaseSnapshot({ permissions: [], questions: [], folderTrustRequests: [], planApprovals: [
    { sessionId: "plan-session", requestId: "closed-plan", toolCallId: "plan-tool", planContent: "历史快照" },
    { sessionId: "plan-session", requestId: "live-plan", toolCallId: "live-tool", planContent: "当前计划" },
  ] });
  const stop = await subscribing;
  try {
    expect(useSessionStore.getState().transcripts["plan-session"].planApprovals.map(p => p.requestId)).toEqual(["live-plan"]);
  } finally {
    stop();
  }
  expect(unlisten).toHaveBeenCalledTimes(1);
});

it("注册中途失败时解除先前监听，重试不会重复分发事件", async () => {
  const unlisten = vi.fn();
  vi.mocked(listen).mockResolvedValueOnce(unlisten).mockRejectedValueOnce(new Error("event unavailable"));
  await expect(subscribeAgentEvents({ onUpdate: vi.fn(), onComplete: vi.fn() })).rejects.toThrow("event unavailable");
  expect(unlisten).toHaveBeenCalledTimes(1);
});

it("初始化快照失败保留成功注册的事件监听直到正常退出", async () => {
  const unlisten = vi.fn();
  vi.mocked(listen).mockResolvedValue(unlisten);
  vi.mocked(invoke).mockRejectedValue(new Error("snapshot unavailable"));
  const stop = await subscribeAgentEvents({ onComplete: vi.fn() });
  expect(unlisten).not.toHaveBeenCalled();
  stop();
  expect(unlisten).toHaveBeenCalledTimes(1);
});
