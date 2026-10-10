import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueuePanel } from "../QueuePanel";
import { useMessageQueueStore } from "@/stores/message-queue-store";

const resetStore = () => useMessageQueueStore.setState({ queues: {} });

describe("QueuePanel", () => {
  beforeEach(resetStore);

  it("空队列时不渲染", () => {
    const { container } = render(<QueuePanel sessionId="s1" />);
    expect(container.firstChild).toBeNull();
  });

  it("渲染队列条目并显示序号与文本", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "第一条");
    s.enqueue("s1", "第二条");
    render(<QueuePanel sessionId="s1" />);
    expect(screen.getByText("待发送队列(2)")).toBeInTheDocument();
    expect(screen.getByText("第一条")).toBeInTheDocument();
    expect(screen.getByText("第二条")).toBeInTheDocument();
  });

  it("删除按钮从队列移除条目", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "第一条");
    render(<QueuePanel sessionId="s1" />);
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    expect(useMessageQueueStore.getState().getQueue("s1")).toHaveLength(0);
  });

  it("点击文本进入编辑,Enter 提交修改", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "原文");
    render(<QueuePanel sessionId="s1" />);
    fireEvent.click(screen.getByText("原文"));
    const edit = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.change(edit, { target: { value: "改后" } });
    fireEvent.keyDown(edit, { key: "Enter", shiftKey: false });
    expect(useMessageQueueStore.getState().getQueue("s1")[0].text).toBe("改后");
  });

  it("暂停/恢复切换状态", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "条目");
    render(<QueuePanel sessionId="s1" />);
    const toggle = screen.getByRole("button", { name: "暂停" });
    fireEvent.click(toggle);
    expect(useMessageQueueStore.getState().getQueue("s1")[0].status).toBe("paused");
    // 切换后按钮文案变为「恢复」。
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    expect(useMessageQueueStore.getState().getQueue("s1")[0].status).toBe("queued");
  });

  it("上移/下移调整顺序", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    s.enqueue("s1", "b");
    render(<QueuePanel sessionId="s1" />);
    // 第二条上移。
    const ups = screen.getAllByRole("button", { name: "上移" });
    fireEvent.click(ups[1]);
    expect(
      useMessageQueueStore.getState().getQueue("s1").map((i) => i.text),
    ).toEqual(["b", "a"]);
  });

  it("立即发送:接纳后保留原条目，直到对应轮次完成", async () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "马上发");
    const onSendNow = vi.fn((_text: string, _attachments?: string[], queueItemId?: string) => {
      useMessageQueueStore.getState().claimById("s1", queueItemId!, "prompt-1");
      return true;
    });
    render(<QueuePanel sessionId="s1" onSendNow={onSendNow} />);
    fireEvent.click(screen.getByRole("button", { name: "立即发送" }));
    expect(onSendNow).toHaveBeenCalledWith("马上发", [], id);
    await waitFor(() => {
      expect(useMessageQueueStore.getState().getQueue("s1")[0]).toMatchObject({
        id,
        status: "sending",
        promptId: "prompt-1",
      });
    });
    act(() => {
      useMessageQueueStore.getState().settleSending("s1", "consume", "prompt-1");
    });
    expect(useMessageQueueStore.getState().getQueue("s1")).toHaveLength(0);
  });

  it("运行时拒绝或发送失败时保留队列项", async () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "不能丢");
    render(<QueuePanel sessionId="s1" onSendNow={() => Promise.resolve(false)} />);
    fireEvent.click(screen.getByRole("button", { name: "立即发送" }));
    await waitFor(() => {
      expect(useMessageQueueStore.getState().getQueue("s1")).toHaveLength(1);
    });
  });

  it("流式回复时立即发送精确认领用户选择的条目", async () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "a");
    const secondId = s.enqueue("s1", "b");
    const onSendNow = vi.fn((_text: string, _attachments?: string[], queueItemId?: string) => {
      useMessageQueueStore.getState().claimById("s1", queueItemId!, "prompt-b");
      return true;
    });
    render(<QueuePanel sessionId="s1" streaming onSendNow={onSendNow} />);
    const actions = screen.getAllByRole("button", { name: "中断当前回复并立即发送这条" });
    fireEvent.click(actions[1]);
    expect(onSendNow).toHaveBeenCalledWith("b", [], secondId);
    await waitFor(() => {
      expect(useMessageQueueStore.getState().getQueue("s1")).toMatchObject([
        { text: "a", status: "queued" },
        { text: "b", status: "sending", promptId: "prompt-b" },
      ]);
    });
  });

  it("sendNow 交接中禁用重复立即发送", () => {
    useMessageQueueStore.getState().enqueue("s1", "a");
    render(
      <QueuePanel
        sessionId="s1"
        streaming
        sendNowPending
        onSendNow={vi.fn()}
      />,
    );
    expect(
      screen.getByRole("button", { name: "中断当前回复并立即发送这条" }),
    ).toBeDisabled();
  });

  it("存在结构化提问时禁用队列发送", () => {
    useMessageQueueStore.getState().enqueue("s1", "稍后发送");
    render(<QueuePanel sessionId="s1" awaitingQuestion onSendNow={vi.fn()} />);
    expect(screen.getByRole("button", { name: "立即发送" })).toBeDisabled();
  });

  it("paused 条目的立即发送按钮禁用", () => {
    const s = useMessageQueueStore.getState();
    const id = s.enqueue("s1", "暂停的");
    s.setStatus("s1", id, "paused");
    render(<QueuePanel sessionId="s1" onSendNow={vi.fn()} />);
    expect(screen.getByRole("button", { name: "立即发送" })).toBeDisabled();
  });

  it("已被后台取出的条目显示发送中且不可编辑或删除", () => {
    const s = useMessageQueueStore.getState();
    s.enqueue("s1", "正在发送");
    s.claimNext("s1");
    render(<QueuePanel sessionId="s1" streaming />);

    expect(screen.getByText("正在发送")).toHaveAttribute("title", "发送中");
    expect(screen.getByRole("button", { name: "删除" })).toBeDisabled();
    fireEvent.click(screen.getByText("正在发送"));
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("展示队列附件并在发送时保留", async () => {
    const id = useMessageQueueStore.getState().enqueue("s1", "请优化", ["/tmp/方案.docx"]);
    const onSendNow = vi.fn((_text: string, _attachments?: string[], queueItemId?: string) => {
      useMessageQueueStore.getState().claimById("s1", queueItemId!, "prompt-attachment");
      return true;
    });
    render(<QueuePanel sessionId="s1" onSendNow={onSendNow} />);
    expect(screen.getByText("📎 方案.docx")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "立即发送" }));
    expect(onSendNow).toHaveBeenCalledWith("请优化", ["/tmp/方案.docx"], id);
    await waitFor(() => expect(useMessageQueueStore.getState().getQueue("s1")[0])
      .toMatchObject({ id, attachments: ["/tmp/方案.docx"], status: "sending" }));
  });

  it("中文组合输入的回车和 Escape 保留队列编辑", () => {
    useMessageQueueStore.getState().enqueue("s1", "原文");
    render(<QueuePanel sessionId="s1" />);
    fireEvent.click(screen.getByText("原文"));
    const edit = screen.getByRole("textbox");
    fireEvent.change(edit, { target: { value: "中文修改" } });
    fireEvent.keyDown(edit, { key: "Enter", isComposing: true });
    fireEvent.keyDown(edit, { key: "Escape", isComposing: true });
    expect(screen.getByRole("textbox")).toHaveValue("中文修改");
    expect(useMessageQueueStore.getState().getQueue("s1")[0].text).toBe("原文");
    fireEvent.keyDown(edit, { key: "Enter" });
    expect(useMessageQueueStore.getState().getQueue("s1")[0].text).toBe("中文修改");
  });

  it("不同会话的发送等待互不阻塞，也不会被旧会话完成事件提前解除", async () => {
    useMessageQueueStore.getState().enqueue("s1", "第一会话");
    useMessageQueueStore.getState().enqueue("s2", "第二会话");
    let finishFirst!: () => void;
    let finishSecond!: () => void;
    const onSendNow = vi.fn()
      .mockImplementationOnce(() => new Promise<void>((resolve) => { finishFirst = resolve; }))
      .mockImplementationOnce(() => new Promise<void>((resolve) => { finishSecond = resolve; }));
    const { rerender } = render(<QueuePanel sessionId="s1" onSendNow={onSendNow} />);
    fireEvent.click(screen.getByRole("button", { name: "立即发送" }));
    rerender(<QueuePanel sessionId="s2" onSendNow={onSendNow} />);
    expect(screen.getByRole("button", { name: "立即发送" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "立即发送" }));
    await act(async () => finishFirst());
    expect(screen.getByRole("button", { name: "立即发送" })).toBeDisabled();
    await act(async () => finishSecond());
    expect(screen.getByRole("button", { name: "立即发送" })).toBeEnabled();
  });
});
