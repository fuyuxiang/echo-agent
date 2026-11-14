import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { agentSuggestPrompt, type PromptSuggestionResponse } from "../agent-client";
import { predictionModel, usePromptPrediction } from "../use-prompt-prediction";
import { touchDraft } from "../draft-lifecycle";
import { usePromptPredictionStore } from "@/stores/prompt-prediction-store";
import type { ChatMessage } from "@/stores/session-store";

vi.mock("../agent-client", () => ({ agentSuggestPrompt: vi.fn() }));
const complete = (id: string): ChatMessage => ({ id, promptId: id, role: "assistant", complete: true,
  stopReason: "end_turn", parts: [{ kind: "text", text: "方案已经整理完成" }] });
const initial = () => ({ sessionId: "s1" as string | null, messages: [] as ChatMessage[], busy: false, draft: "" });
let resolve!: (response: PromptSuggestionResponse) => void;
beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(agentSuggestPrompt).mockReset().mockImplementation(() => new Promise(done => { resolve = done; }));
  usePromptPredictionStore.setState({ enabled: true, model: "" });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });
const tick = () => act(async () => { await vi.advanceTimersByTimeAsync(260); });
const answer = async (suggestion: string | null = "按这个方案实现", override?: number) => {
  await act(async () => resolve({ suggestion, generation: override ?? vi.mocked(agentSuggestPrompt).mock.lastCall![1] }));
};

describe("conversation prediction lifecycle", () => {
  it("requests once for a newly completed turn, preserving its session and generation", async () => {
    const { result, rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    await tick();
    expect(agentSuggestPrompt).toHaveBeenCalledOnce();
    expect(agentSuggestPrompt).toHaveBeenCalledWith("s1", expect.any(Number), undefined);
    await answer();
    expect(result.current.prediction?.text).toBe("按这个方案实现");
    rerender({ ...initial(), messages: [complete("p1")] });
    await tick();
    expect(agentSuggestPrompt).toHaveBeenCalledOnce();
  });

  it("does not generate from an opened historical transcript or cancelled/error turns", async () => {
    const { rerender } = renderHook(usePromptPrediction, { initialProps: { ...initial(), messages: [complete("old")] } });
    await tick();
    for (const message of [{ ...complete("cancelled"), stopReason: "cancelled" },
      { ...complete("failed"), stopReason: "error" }, { ...complete("replay"), replayed: true },
      complete("scheduler-fired-nightly"), complete("subagent-completed-child")]) {
      rerender({ ...initial(), messages: [message] });
      await tick();
    }
    expect(agentSuggestPrompt).not.toHaveBeenCalled();
  });

  it.each(["switch", "new-turn", "disable", "draft-edit", "model-change", "dismiss"])("discards an in-flight result after %s", async reason => {
    const { result, rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    const done = { ...initial(), messages: [complete("p1")] };
    rerender(done);
    await tick();
    if (reason === "switch") rerender({ ...initial(), sessionId: "s2" });
    if (reason === "new-turn") rerender({ ...done, busy: true });
    if (reason === "disable") act(() => usePromptPredictionStore.setState({ enabled: false }));
    if (reason === "model-change") act(() => usePromptPredictionStore.setState({ model: "another-small" }));
    if (reason === "draft-edit") act(() => touchDraft("s1")); // Includes typing and deleting back to empty.
    if (reason === "dismiss") act(() => result.current.dismiss());
    await answer();
    expect(result.current.prediction).toBeNull();
    if (reason === "switch") rerender(done);
    if (reason === "new-turn") rerender({ ...initial(), messages: [{ ...complete("p2"), complete: false }], busy: true });
    await tick();
    expect(result.current.prediction).toBeNull();
    expect(agentSuggestPrompt).toHaveBeenCalledOnce();
  });

  it("clears a displayed prediction on a new turn and generates for its completion", async () => {
    const { result, rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    await tick(); await answer();
    expect(result.current.prediction).not.toBeNull();
    rerender({ ...initial(), busy: true, messages: [{ ...complete("p2"), complete: false }] });
    expect(result.current.prediction).toBeNull();
    rerender({ ...initial(), messages: [complete("p2")] });
    await tick(); await answer("补充测试并验证结果");
    expect(result.current.prediction?.turnId).toBe("p2");
    expect(agentSuggestPrompt).toHaveBeenCalledTimes(2);
  });

  it("coalesces queued admission and skips existing drafts", async () => {
    const { rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    rerender({ ...initial(), messages: [complete("p1")], busy: true });
    await tick();
    rerender({ ...initial(), messages: [complete("p2")], draft: "用户自己的草稿" });
    await tick();
    expect(agentSuggestPrompt).not.toHaveBeenCalled();
  });

  it("does not spend a request when input dismisses the prediction during the debounce", async () => {
    const { result, rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    act(() => result.current.dismiss());
    await tick();
    expect(agentSuggestPrompt).not.toHaveBeenCalled();
  });

  it.each([null, "", "\n", "a\nb", "中".repeat(121)])("silently ignores unusable suggestions (%j)", async text => {
    const { result, rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    await tick(); await answer(text);
    expect(result.current.prediction).toBeNull();
  });

  it("rejects a mismatched server generation and gracefully handles unavailable runtimes", async () => {
    const { result, rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    await tick(); await answer("继续验证", 999999);
    expect(result.current.prediction).toBeNull();
    vi.mocked(agentSuggestPrompt).mockRejectedValueOnce(new Error("method not found"));
    rerender({ ...initial(), messages: [complete("p2")] });
    await tick();
    expect(result.current.prediction).toBeNull();
  });

  it("respects disabled preferences without spending a request", async () => {
    usePromptPredictionStore.setState({ enabled: false });
    const { rerender } = renderHook(usePromptPrediction, { initialProps: initial() });
    rerender({ ...initial(), messages: [complete("p1")] });
    await tick();
    expect(agentSuggestPrompt).not.toHaveBeenCalled();
  });
});

it("selects an available lightweight id or leaves model choice to runtime configuration", () => {
  const models = [{ id: "full" }, { id: "configured-alias", remoteModelId: "gpt-4.1-mini" }];
  expect(predictionModel("explicit", models)).toBe("explicit");
  expect(predictionModel("", models)).toBe("configured-alias");
  expect(predictionModel("", [{ id: "full" }])).toBeUndefined();
});
