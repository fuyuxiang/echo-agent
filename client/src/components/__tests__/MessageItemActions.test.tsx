import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

function openReplyItem(name: string, click = true) {
  fireEvent.click(screen.getByRole("button", { name: "更多回复操作" }));
  const item = screen.getByRole("menuitem", { name });
  if (click) fireEvent.click(item);
  return item;
}

describe("assistant message actions", () => {
  it("回复菜单可以从当前轮次在新对话中继续，并说明禁用原因", () => {
    const onContinue = vi.fn();
    const view = renderMessage(completedMessage, {onContinueInNewConversation:onContinue});
    openReplyItem("在新对话中继续");
    expect(onContinue).toHaveBeenCalledOnce();
    view.unmount();
    renderMessage(completedMessage, {onContinueInNewConversation:onContinue,continuationDisabled:true,continuationDisabledReason:"等待执行结束"});
    const item = openReplyItem("在新对话中继续", false);
    expect(item).toBeDisabled();
    expect(item).toHaveAttribute("title","等待执行结束");
  });
  it("只有未闭合推理标签时不提供复制或导出内部文本的入口", () => {
    renderMessage({ id: "reasoning-only", role: "assistant", complete: true, parts: [{ kind: "text", text: "<analysis>内部内容" }] });
    expect(screen.queryByRole("button", { name: "复制纯文本" })).toBeNull();
    expect(screen.queryByRole("group", { name: "回复操作" })).toBeNull();
  });
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
    fireEvent.click(screen.getByRole("button", {name:"赞"}));
    fireEvent.click(screen.getByLabelText("4 星"));
    fireEvent.change(screen.getByPlaceholderText("补充说明(可选)"), { target: { value: "这段备注仍可继续编辑" } });
    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", {name:"踩"}));
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

    const trigger = screen.getByRole("button", { name: "更多回复操作" });
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    const plainItem = screen.getByRole("menuitem", { name: "复制纯文本" });
    const markdownItem = screen.getByRole("menuitem", { name: "复制 Markdown" });
    expect(plainItem).toHaveFocus();

    fireEvent.keyDown(plainItem, { key: "ArrowDown" });
    expect(markdownItem).toHaveFocus();
    fireEvent.click(markdownItem);

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("**第一段**\n\n第二段"));
    expect(screen.queryByRole("menu", { name: "当前回复操作" })).toBeNull();
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

  it("Escape 关闭复制菜单并把焦点还给触发器", async () => {
    renderMessage();
    const trigger = screen.getByRole("button", { name: "更多回复操作" });
    fireEvent.click(trigger);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "当前回复操作" })).toBeNull();
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("导出格式菜单支持键盘导航与 Escape 返回", async () => {
    renderMessage();
    const trigger = screen.getByRole("button", { name: "更多回复操作" });
    openReplyItem("导出本条回复为办公文件");
    const word = screen.getByRole("menuitem", { name: "Word" });
    expect(word).toHaveFocus();
    fireEvent.keyDown(word, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "PDF" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "选择导出格式" })).toBeNull();
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("选择办公格式前说明公式、图表及 Excel 的导出范围，且保留四个可用入口", () => {
    renderMessage();
    openReplyItem("导出本条回复为办公文件");

    const commonNote = screen.getByText("公式和 Mermaid 图表以原始文本保留。图表可从图表菜单下载 SVG。");
    const spreadsheetNote = screen.getByText("Excel：回复含表格时，仅导出表格。");
    expect(commonNote).toBeVisible();
    expect(spreadsheetNote).toBeVisible();
    const items = screen.getAllByRole("menuitem");
    expect(items.map((item) => item.textContent)).toEqual(["Word", "PDF", "Excel", "PPT"]);
    for (const item of items) {
      expect(item).toBeEnabled();
      expect(item).toHaveAccessibleDescription(expect.stringContaining(commonNote.textContent!));
      expect(commonNote.compareDocumentPosition(item) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
    expect(screen.getByRole("menuitem", { name: "Excel" })).toHaveAccessibleDescription(
      `${commonNote.textContent} ${spreadsheetNote.textContent}`,
    );
    expect(commonNote).not.toHaveAttribute("tabindex");
    expect(spreadsheetNote).not.toHaveAttribute("tabindex");
    expect(items[0]).toHaveFocus();
  });

  it("导出说明不改变菜单首尾导航及离开菜单的焦点行为", () => {
    renderMessage();
    openReplyItem("导出本条回复为办公文件");
    const word = screen.getByRole("menuitem", { name: "Word" });
    const ppt = screen.getByRole("menuitem", { name: "PPT" });
    fireEvent.keyDown(word, { key: "End" });
    expect(ppt).toHaveFocus();
    fireEvent.keyDown(ppt, { key: "ArrowDown" });
    expect(word).toHaveFocus();
    fireEvent.keyDown(word, { key: "ArrowUp" });
    expect(ppt).toHaveFocus();
    fireEvent.keyDown(ppt, { key: "Home" });
    expect(word).toHaveFocus();

    const nextAction = screen.getByRole("button", { name: "复制纯文本" });
    act(() => nextAction.focus());
    expect(screen.queryByRole("menu", { name: "选择导出格式" })).toBeNull();
    expect(nextAction).toHaveFocus();
  });

  it("根据终态区分重新生成和重新执行", () => {
    const onRetry = vi.fn();
    const { rerender } = renderMessage(completedMessage, { onRetry, sessionId: "retry-actions" });
    const actions = screen.getByRole("group", { name: "回复操作" });
    expect(within(actions).getAllByRole("button").map(button => button.getAttribute("aria-label"))).toEqual([
      "复制纯文本", "朗读回复", "重新生成", "赞", "踩", "更多回复操作",
    ]);
    expect(screen.getByRole("button", { name: "重新生成" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "更多回复操作" }));
    expect(screen.queryByRole("menuitem", { name: "重新生成" })).toBeNull();

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
    expect(screen.queryByRole("button", { name: "更多回复操作" })).toBeNull();
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

describe("progressive reply actions preserve business scope", () => {
  it("更多菜单朗读后，停止控制直接可用", async () => {
    const { registerTtsProvider, resetVoiceRegistry } = await import("@/lib/voice-contract");
    const stop = vi.fn(), speak = vi.fn(() => stop);
    registerTtsProvider({id:"test-reply-tts",isAvailable:()=>true,speak});
    renderMessage();
    fireEvent.click(screen.getByRole("button",{name:"朗读回复"}));
    expect(speak).toHaveBeenCalledWith("第一段 第二段", "zh-CN", expect.anything());
    fireEvent.click(screen.getByRole("button",{name:"停止朗读"}));
    expect(stop).toHaveBeenCalledOnce();
    resetVoiceRegistry();
  });
  it("办公导出只导出当前回复并登记到当前会话，忙碌状态直接可见", async () => {
    const documents = await import("@/lib/document-export");
    const catalog = await import("@/lib/artifact-catalog");
    let finish!: (value: never) => void;
    const exportSpy = vi.spyOn(documents,"exportOfficeDocument").mockImplementation(() => new Promise(resolve=>{finish=resolve;}));
    const artifact = {id:"exported-reply",path:"/reply-workspace/reply.docx",kind:"document_export",status:"completed",title:"Reply",toolCallId:"receipt"};
    const registerSpy = vi.spyOn(catalog,"registerDocumentArtifact").mockImplementation(()=>(artifact as never));
    const onOpenArtifact = vi.fn();
    renderMessage(completedMessage,{sessionId:"reply-scope",cwd:"/reply-workspace",onOpenArtifact});
    openReplyItem("导出本条回复为办公文件");
    fireEvent.click(screen.getByRole("menuitem",{name:"Word"}));
    expect(exportSpy).toHaveBeenCalledWith("**第一段**", "**第一段**\n\n第二段", "docx", "reply-scope");
    expect(screen.getByRole("status")).toHaveTextContent("正在导出 DOCX");
    await act(async()=>finish({path:"/reply-workspace/reply.docx",format:"docx"} as never));
    expect(registerSpy).toHaveBeenCalledWith(expect.objectContaining({path:"/reply-workspace/reply.docx"}),"reply-scope","**第一段**","/reply-workspace");
    fireEvent.click(screen.getByRole("button",{name:/reply.docx.*查看成果/}));
    expect(onOpenArtifact).toHaveBeenCalledWith(artifact);
    exportSpy.mockRestore(); registerSpy.mockRestore();
  });
});
