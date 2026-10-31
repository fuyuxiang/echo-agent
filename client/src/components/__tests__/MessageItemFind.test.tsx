import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { ThemeProvider } from "../ThemeProvider";
import { MessageItem } from "../MessageItem";
import type { ChatMessage } from "@/stores/session-store";

function renderMessage(message: ChatMessage, findQuery: string) {
  return render(
    <ThemeProvider>
      <MessageItem message={message} streaming={false} findQuery={findQuery} />
    </ThemeProvider>,
  );
}

describe("MessageItem find highlighting", () => {
  it("只高亮用户可见正文，不暴露或计入注入上下文", () => {
    const message = {
      id: "user-find",
      role: "user",
      complete: true,
      parts: [{
        kind: "text",
        text: "<!--EXPERT_PERSONA_BEGIN-->内部关键词<!--EXPERT_PERSONA_END-->\n关键词正文",
      }],
    } as ChatMessage;

    const { container } = renderMessage(message, "关键词");
    expect(container.querySelectorAll(".find-hit")).toHaveLength(1);
    expect(container.querySelector(".find-hit")).toHaveTextContent("关键词");
    expect(container).not.toHaveTextContent("内部关键词");
  });

  it("多个 assistant Markdown 回复段都生成可连续收集的命中", () => {
    const message = {
      id: "assistant-find",
      role: "assistant",
      complete: true,
      parts: [
        { kind: "text", text: "第一处关键词与第二处关键词" },
        { kind: "text", text: "第三处 **关键词**" },
      ],
    } as ChatMessage;

    const { container } = renderMessage(message, "关键词");
    expect(container.querySelectorAll(".find-hit")).toHaveLength(3);
  });

  it("归入阶段的公开进展仍能查找，结束查找后恢复原来的折叠状态", () => {
    const message: ChatMessage = { id: "find-stage-progress", role: "assistant", complete: true, parts: [
      { kind: "text", text: "核验官方票价关键词", streamId: "1" },
      { kind: "tool_call", streamId: "1", toolCall: { toolCallId: "ticket-source", kind: "read", title: "票价来源", status: "completed", content: [] } },
      { kind: "text", text: "最终攻略", streamId: "2" },
    ] };
    const view = (findQuery: string) => <ThemeProvider><MessageItem message={message} streaming={false} findQuery={findQuery} /></ThemeProvider>;
    const { container, rerender } = render(view("关键词"));
    expect(container.querySelector(".execution-process__header")).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelectorAll(".find-hit")).toHaveLength(1);
    expect(container.querySelector(".execution-process__body .find-hit")).toBeVisible();
    rerender(view(""));
    expect(container.querySelector(".execution-process__header")).toHaveAttribute("aria-expanded", "false");
  });
});
