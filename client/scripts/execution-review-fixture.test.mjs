// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import {
  appendExecutionReviewChunk,
  executionReviewFinalMarkdown,
  executionReviewSessionId,
  executionReviewTeamSnapshot,
  finishExecutionReview,
  seedExecutionReview,
} from "./fixtures/execution-review.tsx";
import { useSessionStore } from "../src/stores/session-store";
import { useSubagentStore } from "../src/stores/subagent-store";
import { collectSessionArtifacts } from "../src/lib/session-artifacts";

const tools = () => useSessionStore.getState().messages.flatMap((message) =>
  message.parts.filter((part) => part.kind === "tool_call").map((part) => part.toolCall),
);
const text = () => useSessionStore.getState().messages.flatMap((message) =>
  message.parts.filter((part) => part.kind === "text" || part.kind === "thought").map((part) => part.text),
).join("\n");

describe("isolated execution-review fixture lifecycle", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", "/?surface=execution");
    localStorage.clear();
  });

  it("uses the live store stream and two parallel subagent lifecycle records", () => {
    seedExecutionReview("running");
    const state = useSessionStore.getState();
    expect(state.sessionId).toBe(executionReviewSessionId);
    expect(state.messages.map((message) => message.role)).toEqual(["user", "assistant"]);
    expect(state.streaming).toBe(true);
    expect(state.messages[1].complete).toBe(false);
    expect(tools()).toHaveLength(6);
    expect(tools().filter((tool) => tool.status === "in_progress")).toHaveLength(3);
    expect(useSubagentStore.getState().getForSession(executionReviewSessionId).map((child) => child.status)).toEqual(["running", "running"]);
    expect(executionReviewTeamSnapshot()[0].members).toHaveLength(2);
    const before = text().length;
    expect(appendExecutionReviewChunk()).toBe(true);
    expect(text().length).toBeGreaterThan(before);
    expect(tools().find((tool) => tool.kind === "run_terminal_command").content[0].output).toContain("追加 6");
  });

  it("finishes the same assistant and tools without residual running states", () => {
    seedExecutionReview("running");
    const id = useSessionStore.getState().streamingMessageId;
    expect(finishExecutionReview()).toBe(true);
    const state = useSessionStore.getState();
    expect(state.streaming).toBe(false);
    expect(state.streamingMessageId).toBeNull();
    expect(state.messages[1]).toMatchObject({ id, complete: true, stopReason: "end_turn" });
    expect(tools().every((tool) => tool.status === "completed")).toBe(true);
    expect(useSubagentStore.getState().getForSession(executionReviewSessionId).every((child) => child.status === "completed")).toBe(true);
    expect(text()).toContain(executionReviewFinalMarkdown);
    expect(collectSessionArtifacts(state.messages).map((artifact) => artifact.path)).toContain("/review/isolated-EchoAgent/generated/界面评审与回归验证报告.md");
    expect(appendExecutionReviewChunk()).toBe(false);
    expect(finishExecutionReview()).toBe(false);
  });

  it("retains partial output and identifies the failed tool and child", () => {
    seedExecutionReview("error");
    const state = useSessionStore.getState();
    expect(state.streaming).toBe(false);
    expect(state.messages[1]).toMatchObject({ complete: true, stopReason: "error" });
    expect(state.error).toContain("隔离失败");
    expect(tools().filter((tool) => tool.status === "failed")).toHaveLength(2);
    expect(tools().some((tool) => tool.status === "in_progress")).toBe(false);
    expect(useSubagentStore.getState().getForSession(executionReviewSessionId).map((child) => child.status)).toEqual(["completed", "failed"]);
    expect(text()).toContain("部分结果已保留");
    expect(collectSessionArtifacts(state.messages)).toHaveLength(1);
  });

  it("clears only the isolated session and supplies a genuine empty snapshot", () => {
    seedExecutionReview("complete");
    seedExecutionReview("empty");
    expect(useSessionStore.getState().messages).toEqual([]);
    expect(useSessionStore.getState().streaming).toBe(false);
    expect(useSessionStore.getState().error).toBeNull();
    expect(useSubagentStore.getState().getForSession(executionReviewSessionId)).toEqual([]);
    expect(executionReviewTeamSnapshot()).toEqual([]);
  });

  it("allows deterministic snapshot failure without reaching a real service", () => {
    seedExecutionReview("complete");
    window.history.replaceState(null, "", "/?surface=execution&teamState=error");
    expect(() => executionReviewTeamSnapshot()).toThrow("隔离快照读取失败");
  });
});
