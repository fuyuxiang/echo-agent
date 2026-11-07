import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { HomePage } from "../HomePage";
import { usePendingExpertStore } from "@/stores/pending-expert-store";
import { HOME_DRAFT_KEY, useSessionsStore } from "@/stores/sessions-store";
import { draftAttachments, saveDraftAttachments } from "@/lib/draft-attachments";
import * as agentClient from "@/lib/agent-client";

const base = {
  onSend: vi.fn(),
  streaming: false,
  apiReady: true,
  onOpenSettings: vi.fn(),
  onPlaceholder: vi.fn(),
};

describe("HomePage", () => {
  beforeEach(() => {
    // 测试间重置 store,避免跨用例污染。
    vi.clearAllMocks();
    useSessionsStore.getState().clearDraft(HOME_DRAFT_KEY);
    usePendingExpertStore.setState({ expert: null });
  });
  afterEach(() => vi.restoreAllMocks());

  it("以单一任务问题和输入框作为首页主入口", () => {
    render(<HomePage {...base} />);
    expect(screen.getByRole("heading", { name: "今天想完成什么？" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "输入问题或任务" }))
      .toHaveAttribute("placeholder", "从一个问题、一个想法，或一份文件开始");
    expect(screen.getByRole("textbox")).toHaveValue("");
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
    expect(document.querySelector(".home__description")).toBeNull();
    expect(screen.queryByText(/本地 Agent 工作台/)).toBeNull();
    expect(screen.queryByText(/结合当前工作区、模型和工具/)).toBeNull();
    expect(document.querySelector(".home__brand-mark")).toBeNull();
  });

  it("添加和移除文件时更新引导，保留用户文字，移除最后一份文件后回到输入框", async () => {
    vi.spyOn(agentClient, "filesystemPickFiles")
      .mockResolvedValueOnce(["/work/brief.txt"])
      .mockResolvedValueOnce(["/work/brief.txt", "/work/notes.md"]);
    vi.spyOn(agentClient, "filesystemAttachmentStats").mockImplementation(async paths => ({
      files: paths.map(path => ({ inputPath: path, path, sizeBytes: 32 })),
      rejected: [],
    }));
    render(<HomePage {...base} />);
    const input = screen.getByRole("textbox", { name: "输入问题或任务" });
    fireEvent.change(input, { target: { value: "保留我自己的需求" } });

    const addFile = async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "添加文件" }));
    };
    await addFile();
    await waitFor(() => expect(input).toHaveAttribute("placeholder", "想如何处理这份文件？"));
    await addFile();
    await waitFor(() => expect(input).toHaveAttribute("placeholder", "想基于这些文件完成什么？"));
    expect(screen.getAllByRole("button", { name: "移除附件" })).toHaveLength(2);
    expect(input).toHaveValue("保留我自己的需求");
    expect(useSessionsStore.getState().drafts[HOME_DRAFT_KEY]).toBe("保留我自己的需求");

    fireEvent.click(screen.getAllByRole("button", { name: "移除附件" })[1]);
    expect(input).toHaveAttribute("placeholder", "想如何处理这份文件？");
    fireEvent.click(screen.getByRole("button", { name: "移除附件" }));
    expect(input).toHaveAttribute("placeholder", "从一个问题、一个想法，或一份文件开始");
    expect(input).toHaveFocus();
    expect(input).toHaveValue("保留我自己的需求");
    expect(base.onSend).not.toHaveBeenCalled();
  });

  it("重返首页时恢复附件和引导，发送被拒绝后保留，成功后恢复空白入口", async () => {
    saveDraftAttachments(HOME_DRAFT_KEY, ["/work/brief.txt", "/work/notes.md"]);
    const onSend = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const first = render(<HomePage {...base} onSend={onSend} />);
    expect(screen.getByRole("textbox")).toHaveAttribute("placeholder", "想基于这些文件完成什么？");
    first.unmount();
    render(<HomePage {...base} onSend={onSend} />);
    const input = screen.getByRole("textbox");
    expect(input).toHaveAttribute("placeholder", "想基于这些文件完成什么？");
    expect(screen.getAllByRole("button", { name: "移除附件" })).toHaveLength(2);

    // 文件也可以作为任务起点，占位提示不能混入发送正文。
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "发送" })).toBeEnabled());
    expect(onSend).toHaveBeenCalledExactlyOnceWith("请分析附件。", ["/work/brief.txt", "/work/notes.md"]);
    expect(input).toHaveAttribute("placeholder", "想基于这些文件完成什么？");
    expect(draftAttachments(HOME_DRAFT_KEY)).toHaveLength(2);

    fireEvent.change(input, { target: { value: "整理成项目要点" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(input).toHaveValue(""));
    expect(onSend).toHaveBeenLastCalledWith("整理成项目要点", ["/work/brief.txt", "/work/notes.md"]);
    expect(input).toHaveAttribute("placeholder", "从一个问题、一个想法，或一份文件开始");
    expect(draftAttachments(HOME_DRAFT_KEY)).toEqual([]);
    expect(screen.getByRole("button", { name: "发送" })).toBeDisabled();
  });

  it("模型同步期间保持首页稳定，不显示首次设置页或引导打开设置", () => {
    const onOpenSettings = vi.fn();
    const { rerender } = render(
      <HomePage {...base} apiReady={false} modelLoading onOpenSettings={onOpenSettings} />,
    );

    expect(screen.getByRole("heading", { name: "今天想完成什么？" })).toBeInTheDocument();
    expect(screen.queryByText("先把 EchoAgent 接到你的工作区")).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent("正在同步可用模型…");
    expect(screen.getByRole("textbox")).toBeDisabled();
    expect(screen.getByRole("textbox")).toHaveAttribute("placeholder", "");
    fireEvent.click(screen.getByText("正在同步可用模型…"));
    expect(onOpenSettings).not.toHaveBeenCalled();

    rerender(<HomePage {...base} apiReady onOpenSettings={onOpenSettings} />);
    expect(screen.getByRole("heading", { name: "今天想完成什么？" })).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toBeEnabled();
    expect(screen.getByRole("textbox")).toHaveAttribute("placeholder", "从一个问题、一个想法，或一份文件开始");
    expect(screen.queryByText("正在同步可用模型…")).toBeNull();
  });

  it("模型确实不可用时在首页提供可操作的连接提示", () => {
    const onOpenSettings = vi.fn();
    render(<HomePage {...base} apiReady={false} onOpenSettings={onOpenSettings} />);

    expect(screen.getByRole("heading", { name: "今天想完成什么？" })).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /暂无可用模型/ }));
    expect(onOpenSettings).toHaveBeenCalledOnce();
  });

  it("不渲染内置场景、能力和 prompt 模板", () => {
    render(<HomePage {...base} />);
    expect(screen.queryByRole("tablist", { name: "场景" })).toBeNull();
    expect(screen.queryByText("日常办公")).toBeNull();
    expect(screen.queryByText("文档处理")).toBeNull();
    expect(screen.queryByText("财报分析全流程")).toBeNull();
  });

  it("首页不展示模式胶囊，从 + 菜单按任务开启网页或电脑操作", async () => {
    const onTaskModeChange = vi.fn();
    const { rerender } = render(
      <HomePage
        {...base}
        taskMode="default"
        onTaskModeChange={onTaskModeChange}
      />,
    );

    expect(screen.queryByLabelText("新任务模式")).toBeNull();
    expect(screen.queryByText("Browser Use")).toBeNull();
    expect(screen.queryByText("Computer Use")).toBeNull();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "添加" }));
    });
    fireEvent.click(screen.getByRole("menuitemradio", { name: /操作网页/ }));
    expect(onTaskModeChange).toHaveBeenCalledWith("browser_use");

    rerender(
      <HomePage
        {...base}
        taskMode="computer_use"
        onTaskModeChange={onTaskModeChange}
      />,
    );
    expect(screen.getByText("操作电脑")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "关闭操作电脑" }));
    expect(onTaskModeChange).toHaveBeenLastCalledWith("default");
  });

  it("点击移除按钮清空 pending 专家,且不破坏输入框中的预填文字", async () => {
    usePendingExpertStore.getState().set({
      name: "小坦克",
      prompt: "...",
      description: "...",
      expertId: "tank",
      source: "local",
      quickPrompt: "请帮我开始",
    });

    render(<HomePage {...base} />);
    // 召唤触发 quickPrompt 预填。
    const input = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(input.value).toBe("请帮我开始");
    // 两处 chip 都有 ×。
    const removeButtons = screen.getAllByRole("button", { name: "移除已选专家" });
    expect(removeButtons.length).toBeGreaterThanOrEqual(2);

    // 点击第一个移除按钮(顶部 chip)。
    act(() => { fireEvent.click(removeButtons[0]); });

    // store 清空 → 两处 chip 消失。
    expect(usePendingExpertStore.getState().expert).toBeNull();
    expect(screen.queryByRole("button", { name: "移除已选专家" })).toBeNull();
    // 输入框已预填的文字保留(只撤销专家身份,不破坏用户文本)。
    expect(input.value).toBe("请帮我开始");
  });

  it("dismiss 后再次召唤同一专家时保留用户已有草稿", () => {
    usePendingExpertStore.getState().set({
      name: "小坦克",
      prompt: "...",
      description: "...",
      expertId: "tank",
      source: "local",
      quickPrompt: "请帮我开始",
    });

    render(<HomePage {...base} />);
    const input = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(input.value).toBe("请帮我开始");

    // 第一次 dismiss。
    act(() => {
      fireEvent.click(screen.getAllByRole("button", { name: "移除已选专家" })[0]);
    });
    expect(usePendingExpertStore.getState().expert).toBeNull();

    // 用户编辑一下文本,模拟「dismiss 后自己加了一些字」。
    act(() => { fireEvent.change(input, { target: { value: "用户接着改的字" } }); });
    expect(input.value).toBe("用户接着改的字");

    // 重新召唤同一专家。
    act(() => {
      usePendingExpertStore.getState().set({
        name: "小坦克",
        prompt: "...",
        description: "...",
        expertId: "tank",
        source: "local",
        quickPrompt: "请帮我开始",
      });
    });

    // 已有草稿比专家 quickPrompt 优先，避免用户输入丢失。
    expect(input.value).toBe("用户接着改的字");
  });
});
