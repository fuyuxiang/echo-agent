import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MessageItem } from "../MessageItem";
import { ThemeProvider } from "../ThemeProvider";
import type { ChatMessage } from "@/stores/session-store";
import { useFeedbackStore } from "@/stores/feedback-store";

const originalClipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, "clipboard");

const completedMessage: ChatMessage = {
  id: "assistant-actions",
  role: "assistant",
  complete: true,
  parts: [
    { kind: "text", text: "**第一段**" },
    { kind: "text", text: "第二段" },
  ],
};

function renderMessage(
  message: ChatMessage = completedMessage,
  props: Partial<React.ComponentProps<typeof MessageItem>> = {},
) {
  return render(
    <ThemeProvider>
      <MessageItem message={message} streaming={false} {...props} />
    </ThemeProvider>,
  );
}

describe("assistant message actions", () => {
  afterEach(() => {
    if (originalClipboardDescriptor) {
      Object.defineProperty(navigator, "clipboard", originalClipboardDescriptor);
    } else {
      Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("真实消息反馈关闭重开保留草稿，并使用重新选择的方向提交", () => {
    useFeedbackStore.getState().__replace({});
    renderMessage(completedMessage, { latest: true, sessionId: "feedback-session" });
    fireEvent.click(screen.getByRole("button", { name: "赞" }));
    fireEvent.click(screen.getByLabelText("4 星"));
    fireEvent.change(screen.getByPlaceholderText("补充说明(可选)"), { target: { value: "这段备注仍可继续编辑" } });
    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "踩" }));
    expect(screen.getByText(/这条回复有待改进/)).toBeInTheDocument();
    expect(screen.getByPlaceholderText("补充说明(可选)")).toHaveValue("这段备注仍可继续编辑");
    expect(screen.getByText("较好")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "提交" }));
    expect(useFeedbackStore.getState().getRating("feedback-session", completedMessage.id)).toMatchObject({
      rating: "down", stars: 4, note: "这段备注仍可继续编辑",
    });
  });

  it("把完成后的操作栏放在回复正文之后", () => {
    const { container } = renderMessage(completedMessage, { latest: true });
    const header = container.querySelector(".msg__header");
    const body = container.querySelector(".msg__body");
    const actions = screen.getByRole("group", { name: "回复操作" });

    expect(header?.querySelector(".msg__actions")).toBeNull();
    expect(body).not.toBeNull();
    expect(body!.compareDocumentPosition(actions) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(actions).toHaveClass("msg__actions--footer", "msg__actions--latest");
  });

  it("复制保留一键纯文本入口，Markdown 收入可访问菜单", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    renderMessage();

    const trigger = screen.getByRole("button", { name: "更多复制选项" });
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    const plainItem = screen.getByRole("menuitem", { name: "复制纯文本" });
    const markdownItem = screen.getByRole("menuitem", { name: "复制 Markdown" });
    expect(plainItem).toHaveFocus();

    fireEvent.keyDown(plainItem, { key: "ArrowDown" });
    expect(markdownItem).toHaveFocus();
    fireEvent.click(markdownItem);

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("**第一段**\n\n第二段"));
    expect(screen.queryByRole("menu", { name: "选择复制格式" })).toBeNull();
    expect(screen.getByRole("button", { name: "已复制 Markdown" })).toBeInTheDocument();
  });

  it("默认复制只带正式答案，不泄露折叠的中间过程", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    renderMessage({
      id: "assistant-process-copy",
      role: "assistant",
      complete: true,
      parts: [
        {
          kind: "text",
          text: "<think>I should inspect the project first.</think>\n\n这是用户需要的正式答案。",
          streamId: "generation-1",
        },
      ],
    });

    fireEvent.click(screen.getByRole("button", { name: "复制纯文本" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("这是用户需要的正式答案。"));
    expect(writeText).not.toHaveBeenCalledWith(expect.stringContaining("I should inspect"));
  });

  it("Escape 关闭复制菜单并把焦点还给触发器", () => {
    renderMessage();
    const trigger = screen.getByRole("button", { name: "更多复制选项" });
    fireEvent.click(trigger);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "选择复制格式" })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("导出格式菜单支持键盘导航与 Escape 返回", () => {
    renderMessage();
    const trigger = screen.getByRole("button", { name: "导出回复为办公文件" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const word = screen.getByRole("menuitem", { name: "Word" });
    expect(word).toHaveFocus();
    fireEvent.keyDown(word, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "PDF" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "选择导出格式" })).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("根据终态区分重新生成和重新执行", () => {
    const onRetry = vi.fn();
    const { rerender } = renderMessage(completedMessage, { onRetry });
    expect(screen.getByRole("button", { name: "重新生成" })).toBeEnabled();

    rerender(
      <ThemeProvider>
        <MessageItem
          message={{
            id: "failed-actions",
            role: "assistant",
            complete: true,
            parts: [],
            stopReason: "error",
          }}
          streaming={false}
          onRetry={onRetry}
          retrying
        />
      </ThemeProvider>,
    );
    const retry = screen.getByRole("button", { name: "正在重试…" });
    expect(retry).toBeDisabled();
    expect(retry).toHaveAttribute("aria-busy", "true");
    expect(screen.queryByRole("button", { name: "复制纯文本" })).toBeNull();
  });

  it("只要本轮使用过工具，就明确标记为重新执行", () => {
    const onRetry = vi.fn();
    renderMessage({
      id: "assistant-tool-actions",
      role: "assistant",
      complete: true,
      parts: [
        {
          kind: "tool_call",
          toolCall: {
            toolCallId: "tool-1",
            title: "运行终端命令",
            kind: "terminal",
            status: "completed",
            content: [],
          },
        },
        { kind: "text", text: "命令已完成。" },
      ],
    }, { onRetry });

    const button = screen.getByRole("button", { name: "重新执行" });
    expect(button).toHaveAttribute("title", "重新执行本轮任务（可能再次调用工具）");
    fireEvent.click(button);
    expect(onRetry).toHaveBeenCalledWith("reexecute");
  });

  it("流式回复完成前不显示读后操作", () => {
    renderMessage({ ...completedMessage, complete: false }, { streaming: true });
    expect(screen.queryByRole("group", { name: "回复操作" })).toBeNull();
  });
});
