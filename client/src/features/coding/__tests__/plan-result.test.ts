import { describe, expect, it } from "vitest";
import type { ChatMessage } from "@/stores/session-store";
import { codingPlanResult } from "../lib/plan-result";

const prompt: ChatMessage = { id: "plan", role: "user", complete: true,
  agentText: "[回声代码 · plan · 继续当前会话]\n规划", parts: [{ kind: "text", text: "规划" }] };
const answer: ChatMessage = { id: "answer", role: "assistant", complete: true,
  stopReason: "end_turn", parts: [{ kind: "text", text: "<think>Internal process</think>1. 读取\n2. 修改并验证" }] };

describe("completed coding plans", () => {
  it("adopts only the public answer of the latest successful Plan", () => {
    expect(codingPlanResult([prompt, answer])).toEqual({ status: "complete", text: "1. 读取\n2. 修改并验证" });
    expect(codingPlanResult([prompt, answer, { ...prompt, id: "ask", agentText: "[回声代码 · ask]\n解释" }]))
      .toEqual({ status: "none", text: "" });
  });

  it.each(["cancelled", "error", "max_tokens", "max_turns", "refusal", "content_filter", "rate_limit"])(
    "rejects a terminated partial Plan with reason %s", stopReason => {
      expect(codingPlanResult([prompt, { ...answer, stopReason, parts: [{ kind: "text", text: "1. 修改\n2. ", phase: "partial_answer" }] }]))
        .toEqual({ status: "interrupted", text: "" });
    },
  );

  it("waits for a live response and rejects history without an authoritative completion", () => {
    expect(codingPlanResult([prompt, { ...answer, complete: false }])).toEqual({ status: "generating", text: "" });
    expect(codingPlanResult([prompt, { ...answer, replayed: true, stopReason: undefined }]))
      .toEqual({ status: "interrupted", text: "" });
    expect(codingPlanResult([prompt, { ...answer, stopReason: undefined }]).status).toBe("complete");
    expect(codingPlanResult([prompt, { ...answer, replayed: true }]).status).toBe("complete");
  });
});
