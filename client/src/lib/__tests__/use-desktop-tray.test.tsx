import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useDesktopTray } from "../use-desktop-tray";
import { useSessionsStore } from "@/stores/sessions-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useQuestionStore } from "@/stores/question-store";
import { useGroupStore } from "@/features/groups/group-store";

const native = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Map<string, (event: { payload: unknown }) => void>(),
  approvals: vi.fn<() => Promise<Array<{ requestId: string; sessionId: string }>>>() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (event: string, callback: (event: { payload: unknown }) => void) => {
  native.listeners.set(event, callback); return () => native.listeners.delete(event);
}) }));
vi.mock("../automation-client", () => ({
  automationPendingApprovals: native.approvals,
  onAutomationApproval: async (callback: (value: unknown) => void) => {
    native.listeners.set("automation://approval", event => callback(event.payload)); return () => native.listeners.delete("automation://approval");
  },
  onAutomationApprovalClosed: async (callback: (value: unknown) => void) => {
    native.listeners.set("automation://approval-closed", event => callback(event.payload)); return () => native.listeners.delete("automation://approval-closed");
  },
}));

describe("desktop tray live state", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    native.listeners.clear(); native.invoke.mockReset().mockResolvedValue(null); native.approvals.mockReset().mockResolvedValue([]);
    useSessionsStore.setState({ independent: [{ sessionId: "background", title: "后台浏览器任务", cwd: "/tmp", status: "working" }], currentSessionId: null });
    usePermissionStore.getState().clearAll(); useQuestionStore.getState().clearAll();
    useGroupStore.setState({ groups: [], current: null, currentId: null });
  });
  afterEach(() => { delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__; });
  const latest = () => native.invoke.mock.calls.filter(call => call[0] === "desktop_tray_sync").slice(-1)[0]?.[1].snapshot;

  it("shows background automation approvals and prevents delayed events resurrecting closed requests", async () => {
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(latest()?.runningCount).toBe(1));
    act(() => native.listeners.get("automation://approval")!({ payload: { requestId: "approval", sessionId: "background" } }));
    await waitFor(() => expect(latest()?.waitingCount).toBe(1));
    expect(latest().runningCount).toBe(0);
    act(() => native.listeners.get("automation://approval-closed")!({ payload: { requestId: "approval", sessionId: "background" } }));
    await waitFor(() => expect(latest()?.runningCount).toBe(1));
    act(() => native.listeners.get("automation://approval")!({ payload: { requestId: "approval", sessionId: "background" } }));
    expect(latest().waitingCount).toBe(0);
    view.unmount(); expect(native.listeners.size).toBe(0);
  });
  it("a completed stale hydration cannot replace a newer approval event", async () => {
    let finish!: (value: Array<{ requestId: string; sessionId: string }>) => void;
    native.approvals.mockReturnValue(new Promise(resolve => { finish = resolve; }));
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(native.approvals).toHaveBeenCalled());
    act(() => native.listeners.get("automation://approval")!({ payload: { requestId: "new", sessionId: "background" } }));
    await act(async () => finish([]));
    await waitFor(() => expect(latest()?.waitingCount).toBe(1));
    view.unmount();
  });
  it("removes deleted tasks from the native menu even while the window is hidden", async () => {
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(latest()?.tasks).toHaveLength(1));
    act(() => useSessionsStore.setState({ independent: [] }));
    await waitFor(() => expect(latest()?.tasks).toEqual([]));
    view.unmount();
  });

  it("merges existing approvals with events received during hydration", async () => {
    let finish!: (value: Array<{ requestId: string; sessionId: string }>) => void;
    native.approvals.mockReturnValue(new Promise(resolve => { finish = resolve; }));
    useSessionsStore.setState({ independent: [
      { sessionId: "old-session", title: "已有审批", cwd: "/tmp", status: "working" },
      { sessionId: "background", title: "新审批", cwd: "/tmp", status: "working" },
    ] });
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(native.approvals).toHaveBeenCalled());
    act(() => native.listeners.get("automation://approval")!({ payload: { requestId: "new", sessionId: "background" } }));
    await act(async () => finish([{ requestId: "old", sessionId: "old-session" }]));
    await waitFor(() => expect(latest()?.waitingCount).toBe(2));
    expect(latest().runningCount).toBe(0);
    view.unmount();
  });

  it("does not resurrect an approval closed while its snapshot is loading", async () => {
    let finish!: (value: Array<{ requestId: string; sessionId: string }>) => void;
    native.approvals.mockReturnValue(new Promise(resolve => { finish = resolve; }));
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(native.approvals).toHaveBeenCalled());
    act(() => native.listeners.get("automation://approval-closed")!({ payload: { requestId: "closed", sessionId: "background" } }));
    await act(async () => finish([{ requestId: "closed", sessionId: "background" }]));
    await waitFor(() => expect(latest()?.runningCount).toBe(1));
    expect(latest().waitingCount).toBe(0);
    view.unmount();
  });

  it("an older focus refresh cannot overwrite a newer recovery snapshot", async () => {
    const finishes: Array<(value: Array<{ requestId: string; sessionId: string }>) => void> = [];
    native.approvals.mockImplementation(() => new Promise(resolve => { finishes.push(resolve); }));
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(finishes).toHaveLength(1));
    act(() => window.dispatchEvent(new Event("focus")));
    await waitFor(() => expect(finishes).toHaveLength(2));
    await act(async () => finishes[1]([]));
    await act(async () => finishes[0]([{ requestId: "stale", sessionId: "background" }]));
    expect(latest().waitingCount).toBe(0);
    view.unmount();
  });

  it("removes old missing requests while preserving concurrent new requests", async () => {
    native.approvals.mockResolvedValue([{ requestId: "old", sessionId: "background" }]);
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(latest()?.waitingCount).toBe(1));
    let finish!: (value: Array<{ requestId: string; sessionId: string }>) => void;
    native.approvals.mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
    act(() => window.dispatchEvent(new Event("focus")));
    await waitFor(() => expect(native.approvals).toHaveBeenCalledTimes(2));
    act(() => native.listeners.get("automation://approval")!({ payload: { requestId: "new", sessionId: "background" } }));
    await act(async () => finish([]));
    expect(latest().waitingCount).toBe(1);
    act(() => native.listeners.get("automation://approval-closed")!({ payload: { requestId: "new", sessionId: "background" } }));
    await waitFor(() => expect(latest()?.waitingCount).toBe(0));
    view.unmount();
  });

  it("prefers a newer group catalog over the stale selected group snapshot", async () => {
    const group = { id: "group", name: "产品评审", members: [], works: [], updatedAt: 1, revision: 1, archived: false };
    useGroupStore.setState({ groups: [{ ...group, archived: true, revision: 2 }], current: group } as never);
    const view = renderHook(() => useDesktopTray(true, vi.fn(), vi.fn()));
    await waitFor(() => expect(latest()).toBeDefined());
    expect(latest().tasks.every((task: { kind: string }) => task.kind !== "group")).toBe(true);
    view.unmount();
  });
});
