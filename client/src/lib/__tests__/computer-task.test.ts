import { describe, expect, it } from "vitest";
import { automationStatusForMessage } from "../computer-task";
import type { AutomationStatus } from "../automation-client";
import type { ChatMessage } from "@/stores/session-store";

const message = (id: string, promptId?: string): ChatMessage => ({ id, promptId, role: "assistant", complete: true, parts: [] });
const status: AutomationStatus = {
  sessionId: "session", mode: "computer_use", paused: false, allowPrivateNetwork: false,
  browser: { available: true }, computer: { available: true, platform: "macOS", screenCapture: true, inputControl: true },
  browserRunning: false, browserHasData: false,
  task: { phase: "running", reason: null, scope: "核对来源", authorized: true, generation: 1, promptId: "current" },
};

describe("native task ownership in the transcript", () => {
  it("only attaches to the last assistant of the owning prompt, even while a later prompt is visible", () => {
    const old = message("old", "old"), first = message("first", "current"), last = message("last", "current"), newer = message("newer", "newer");
    const messages = [old, first, last, newer];
    expect(automationStatusForMessage(last, messages, status, true)).toBe(status);
    for (const other of [old, first, newer]) expect(automationStatusForMessage(other, messages, status)).toBeUndefined();
  });

  it("legacy unowned tasks only attach to the final assistant while the conversation is idle", () => {
    const first = message("first"), last = message("last");
    const legacy = { ...status, task: { ...status.task!, promptId: undefined } };
    expect(automationStatusForMessage(last, [first, last], legacy)).toBe(legacy);
    expect(automationStatusForMessage(last, [first, last], legacy, true)).toBeUndefined();
    expect(automationStatusForMessage(first, [first, last], legacy)).toBeUndefined();
    const user = { ...last, role: "user" as const };
    expect(automationStatusForMessage(user, [first, user], legacy)).toBeUndefined();
    expect(automationStatusForMessage(last, [last], { ...status, task: { ...status.task!, scope: null } })).toBeUndefined();
  });
});
