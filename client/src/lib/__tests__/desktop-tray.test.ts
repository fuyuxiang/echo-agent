import { describe, expect, it, vi } from "vitest";
import { buildTraySnapshot, createTrayPublisher, recordTrayVisit, type TraySnapshot } from "../desktop-tray-model";
import { connectDesktopTray, TrayActionCancelled } from "../desktop-tray-connection";
import type { SessionSummary } from "../types";
import type { Group } from "@/features/groups/group-client";

const session = (id: string, status: SessionSummary["status"] = "completed", updatedAt = "2026-10-30T10:00:00Z"): SessionSummary =>
  ({ sessionId: id, title: id, cwd: "/workspace", status, updatedAt });
const empty: TraySnapshot = { waitingCount: 0, runningCount: 0, tasks: [] };
const deferred = () => { let resolve!: () => void; const promise = new Promise<void>(done => { resolve = done; }); return { promise, resolve }; };

describe("desktop tray task model", () => {
  it("prioritizes attention, running and recent tasks while counting beyond three rows", () => {
    const snapshot = buildTraySnapshot([session("recent"), session("run", "planning"), session("ask", "awaiting_answer"), session("other", "working")], [], new Set(), {});
    expect(snapshot).toMatchObject({ waitingCount: 1, runningCount: 2 });
    expect(snapshot.tasks.map(task => task.id)).toEqual(["ask", "other", "run"]);
  });
  it("uses visits rather than background activity for recently opened tasks", () => {
    const visits = recordTrayVisit({}, { kind: "session", id: "old" }, Date.parse("2026-10-30T12:00:00Z"));
    const snapshot = buildTraySnapshot([session("new"), session("old", "paused", "2020-01-01")], [], new Set(), visits);
    expect(snapshot.tasks[0].id).toBe("old");
    expect(snapshot.runningCount).toBe(0);
    expect(Object.keys(recordTrayVisit(Object.fromEntries(Array.from({ length: 100 }, (_, i) => [`session:${i}`, i])), { kind: "session", id: "latest" }, 1000))).toHaveLength(100);
  });
  it("excludes archived, hidden and child sessions and deduplicates session ids", () => {
    const snapshot = buildTraySnapshot([session("one"), session("one"), { ...session("hidden", "working"), hidden: true },
      { ...session("archived", "awaiting_permission"), archived: true }, { ...session("child", "working"), sessionKind: "subagent" }], [], new Set(), {});
    expect(snapshot.tasks.map(task => task.id)).toEqual(["one"]);
    expect(snapshot.runningCount + snapshot.waitingCount).toBe(0);
  });
  it("pending permission events win over a delayed running status", () => {
    expect(buildTraySnapshot([session("pending", "working")], [], new Set(["pending"]), {}))
      .toMatchObject({ waitingCount: 1, runningCount: 0, tasks: [{ state: "waiting" }] });
  });
  it("counts a group once and routes it as a group instead of exposing member sessions", () => {
    const group = { id: "team", name: "产品评审", archived: false, updatedAt: 1,
      members: [{ sessionId: "member", discussionSessionId: "discussion" }], works: [{ sessionId: "member", status: "running" }] } as Group;
    const snapshot = buildTraySnapshot([session("member", "working"), session("discussion", "working")], [group], new Set(["member"]), {});
    expect(snapshot).toEqual({ waitingCount: 1, runningCount: 0, tasks: [{ kind: "group", id: "team", title: "产品评审", state: "waiting" }] });
    expect(buildTraySnapshot([], [{ ...group, archived: true }], new Set(), {})).toEqual(empty);
  });
  it("new installs have no invented tasks or running state", () => {
    expect(buildTraySnapshot([], [], new Set(), {})).toEqual(empty);
  });
  it("serializes native updates and retains the newest pending snapshot", async () => {
    const gate = deferred();
    const send = vi.fn().mockImplementationOnce(() => gate.promise).mockResolvedValue(undefined);
    const publish = createTrayPublisher(send, vi.fn());
    publish.publish(empty);
    publish.publish({ ...empty, waitingCount: 1 });
    publish.publish({ ...empty, waitingCount: 2 });
    expect(send).toHaveBeenCalledTimes(1);
    gate.resolve();
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(2));
    expect(send.mock.calls[1][0].waitingCount).toBe(2);
    publish.publish({ ...empty, waitingCount: 2 });
    await Promise.resolve(); expect(send).toHaveBeenCalledTimes(2);
  });
});

describe("desktop tray navigation recovery", () => {
  function setup(onAction = vi.fn().mockResolvedValue(undefined)) {
    let pending: { id: string; kind: "new" } | null = { id: "1", kind: "new" };
    let listener = () => {};
    const invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      if (command === "desktop_tray_pending_action") return pending;
      if (command === "desktop_tray_ack_action" && pending?.id === args?.id) { pending = null; return true; }
      return undefined;
    });
    const stop = vi.fn(), onError = vi.fn();
    const connection = connectDesktopTray({ invoke: invoke as never, listen: async (_, callback) => { listener = callback; return stop; },
      snapshot: () => empty, onAction, onError });
    return { invoke, stop, onError, connection, onAction, click: () => listener() };
  }
  it("recovers a click queued before React installed its event listener", async () => {
    const state = setup();
    await vi.waitFor(() => expect(state.invoke).toHaveBeenCalledWith("desktop_tray_ack_action", { id: "1" }));
    expect(state.onAction).toHaveBeenCalledWith({ id: "1", kind: "new" });
    state.click(); await Promise.resolve(); expect(state.onAction).toHaveBeenCalledTimes(1);
    state.connection.dispose(); expect(state.stop).toHaveBeenCalledOnce();
  });
  it("keeps failed navigation available for explicit retry", async () => {
    const onAction = vi.fn().mockRejectedValueOnce(new Error("unsaved work")).mockResolvedValue(undefined);
    const state = setup(onAction);
    await vi.waitFor(() => expect(state.onError).toHaveBeenCalledOnce());
    expect(state.invoke.mock.calls.some(call => call[0] === "desktop_tray_ack_action")).toBe(false);
    state.onError.mock.calls[0][1]();
    await vi.waitFor(() => expect(state.invoke).toHaveBeenCalledWith("desktop_tray_ack_action", { id: "1" }));
    state.connection.dispose();
  });
  it("does not acknowledge an in-flight navigation after the renderer disconnects", async () => {
    const gate = deferred(); const state = setup(vi.fn(() => gate.promise));
    await vi.waitFor(() => expect(state.onAction).toHaveBeenCalledOnce());
    state.connection.dispose(); gate.resolve();
    await Promise.resolve(); await Promise.resolve();
    expect(state.invoke.mock.calls.some(call => call[0] === "desktop_tray_ack_action")).toBe(false);
  });
  it("cancellation consumes the click without reopening the unsaved-work dialog on focus", async () => {
    const state = setup(vi.fn().mockRejectedValue(new TrayActionCancelled()));
    await vi.waitFor(() => expect(state.invoke).toHaveBeenCalledWith("desktop_tray_ack_action", { id: "1" }));
    expect(state.onError).not.toHaveBeenCalled();
    state.connection.wake(); await Promise.resolve();
    expect(state.onAction).toHaveBeenCalledOnce(); state.connection.dispose();
  });
});
