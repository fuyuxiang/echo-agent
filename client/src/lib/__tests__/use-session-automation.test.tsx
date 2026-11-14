import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AutomationStatus } from "../automation-client";
import { useSessionAutomation } from "../use-session-automation";
const transport = vi.hoisted(() => ({
  status: vi.fn(), handler: null as ((status: AutomationStatus) => void) | null,
}));
vi.mock("../automation-client", () => ({
  automationStatus: (...args: unknown[]) => transport.status(...args),
  automationPendingApprovals: async () => [],
  onAutomationStatus: async (handler: (status: AutomationStatus) => void) => { transport.handler = handler; return () => {}; },
  onAutomationApproval: async () => () => {},
  onAutomationApprovalClosed: async () => () => {},
}));
const status = (sessionId: string, revision: number, paused = false) => ({
  sessionId, revision, paused, mode: "computer_use",
} as AutomationStatus);
beforeEach(() => {
  Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  transport.status.mockReset(); transport.handler = null;
});
afterEach(() => { Reflect.deleteProperty(window, "__TAURI_INTERNALS__"); });

describe("task status ordering", () => {
  it("does not let an older event undo a paused snapshot obtained by refresh", async () => {
    transport.status.mockResolvedValue(status("a", 7, true));
    const { result } = renderHook(() => useSessionAutomation("a"));
    await waitFor(() => expect(result.current.status?.paused).toBe(true));
    act(() => transport.handler?.(status("a", 6, false)));
    expect(result.current.status?.paused).toBe(true);
    act(() => transport.handler?.(status("a", 8, false)));
    expect(result.current.status?.paused).toBe(false);
  });
  it("discards a late snapshot after the user switches tasks", async () => {
    let complete!: (status: AutomationStatus) => void;
    transport.status.mockImplementation((id: string) => id === "a"
      ? new Promise(resolve => { complete = resolve; }) : Promise.resolve(status("b", 1, true)));
    const { result, rerender } = renderHook(({ id }) => useSessionAutomation(id), { initialProps: { id: "a" } });
    await waitFor(() => expect(complete).toBeDefined());
    rerender({ id: "b" });
    await waitFor(() => expect(result.current.status?.sessionId).toBe("b"));
    await act(async () => complete(status("a", 50)));
    expect(result.current.status?.sessionId).toBe("b");
    expect(result.current.status?.paused).toBe(true);
  });
});
