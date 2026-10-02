// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";

const api = vi.hoisted(() => ({
  memoryList: vi.fn(),
  memoryAppend: vi.fn(),
  memoryClearSessionSummaries: vi.fn(),
  memorySave: vi.fn(),
  memoryDelete: vi.fn(),
  memoryFlush: vi.fn(),
  memoryDream: vi.fn(),
  memoryRewrite: vi.fn(),
}));

vi.mock("@/lib/agent-client", () => api);

import { ResourcesPanel } from "../ResourcesPanel";

const GLOBAL_ENTRY = {
  scope: "global" as const,
  path: "MEMORY.md",
  content: "# Global\n\nOriginal",
  size: 19,
  revision: "rev-global",
  modifiedAt: "2026-08-31T00:00:00Z",
  readOnly: false,
};

const SESSION_ENTRY = {
  scope: "session" as const,
  path: "2026-08-31-session.md",
  content: "# Session log",
  size: 13,
  revision: "rev-session",
  modifiedAt: "2026-08-31T00:00:00Z",
  readOnly: true,
};

describe("ResourcesPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.memoryList.mockResolvedValue([GLOBAL_ENTRY, SESSION_ENTRY]);
    api.memoryAppend.mockResolvedValue(GLOBAL_ENTRY);
    api.memorySave.mockResolvedValue(GLOBAL_ENTRY);
    api.memoryDelete.mockResolvedValue(undefined);
    api.memoryClearSessionSummaries.mockResolvedValue(2);
    api.memoryFlush.mockResolvedValue(true);
    api.memoryDream.mockResolvedValue(true);
    api.memoryRewrite.mockResolvedValue("# Global\n\nRewritten");
  });

  it("将长期记忆与会话摘要分开呈现，摘要只读但可删除", async () => {
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" />);

    expect(await screen.findByText("MEMORY.md")).toBeInTheDocument();
    expect(screen.queryByText("2026-08-31-session.md")).not.toBeInTheDocument();
    expect(screen.getByLabelText("当前记忆上下文")).toHaveTextContent("当前工作区/repo已连接当前会话");
    await user.click(screen.getByRole("tab", { name: /会话摘要/ }));
    expect(screen.getByText("2026-08-31-session.md")).toBeInTheDocument();
    expect(screen.getByRole("note")).toHaveTextContent("不是完整聊天记录");
    expect(screen.getByTitle("删除摘要")).toBeInTheDocument();

    fireEvent.click(screen.getByTitle("查看"));
    expect(screen.getByDisplayValue("# Session log")).toHaveAttribute("readonly");
    expect(screen.queryByRole("button", { name: "保存" })).not.toBeInTheDocument();
  });

  it("落盘携带当前会话 ID", async () => {
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" />);
    await screen.findByText("MEMORY.md");

    fireEvent.click(screen.getByRole("tab", { name: /会话摘要/ }));
    fireEvent.click(screen.getByRole("button", { name: "立即提取" }));
    await waitFor(() => expect(api.memoryFlush).toHaveBeenCalledWith("session-1"));
  });

  it("提取被跳过时不显示保存成功", async () => {
    const onToast = vi.fn();
    api.memoryFlush.mockResolvedValueOnce(false);
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" onToast={onToast} />);
    await screen.findByText("MEMORY.md");
    fireEvent.click(screen.getByRole("tab", { name: /会话摘要/ }));
    fireEvent.click(screen.getByRole("button", { name: "立即提取" }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("本次提取未完成，请稍后重试"));
  });

  it("没有可整理内容时不显示长期记忆已更新", async () => {
    const user = userEvent.setup();
    const onToast = vi.fn();
    api.memoryDream.mockResolvedValueOnce(false);
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" onToast={onToast} />);
    await screen.findByText("MEMORY.md");
    await user.click(screen.getByRole("tab", { name: /会话摘要/ }));
    await user.click(screen.getByRole("button", { name: /整理到长期记忆/ }));
    await user.click(within(screen.getByRole("dialog", { name: "整理历史会话摘要？" })).getByRole("button", { name: "开始整理" }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("本次没有更新长期记忆；可能没有待整理的摘要"));
  });

  it("没有活动会话时明确说明维护能力受限", async () => {
    render(<ResourcesPanel cwd="/repo" />);
    await screen.findByText("MEMORY.md");

    expect(screen.getByLabelText("当前记忆上下文")).toHaveTextContent("未连接会话");
    fireEvent.click(screen.getByRole("tab", { name: /会话摘要/ }));
    expect(screen.queryByRole("button", { name: "立即提取" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /整理到长期记忆/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "清空摘要" })).toBeInTheDocument();
  });

  it("旧版摘要中的模型推理不出现在预览、搜索和查看内容中", async () => {
    api.memoryList.mockResolvedValueOnce([{ ...SESSION_ENTRY, content: "<think>private reasoning</think>\n## 可复用信息\n\n用户喜欢简洁说明" }]);
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" />);
    await user.click(await screen.findByRole("tab", { name: /会话摘要/ }));

    expect(screen.getByText(/用户喜欢简洁说明/)).toBeInTheDocument();
    expect(screen.queryByText(/private reasoning/)).not.toBeInTheDocument();
    await user.type(screen.getByRole("textbox", { name: "搜索记忆" }), "private reasoning");
    expect(screen.queryByText("2026-08-31-session.md")).not.toBeInTheDocument();
    await user.clear(screen.getByRole("textbox", { name: "搜索记忆" }));
    await user.click(screen.getByTitle("查看"));
    expect(screen.getByDisplayValue(/用户喜欢简洁说明/)).toHaveAttribute("readonly");
    expect(screen.queryByDisplayValue(/private reasoning/)).not.toBeInTheDocument();
  });

  it("搜索无结果时说明原因并能一键恢复列表", async () => {
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" />);
    await screen.findByText("MEMORY.md");

    await user.type(screen.getByRole("textbox", { name: "搜索记忆" }), "不存在的关键词");
    expect(screen.getByRole("status")).toHaveTextContent("没有找到匹配的长期记忆文档");
    expect(screen.queryByText(/暂无长期记忆文档/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "清除搜索" }));
    expect(screen.getByText("MEMORY.md")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "搜索记忆" })).toHaveValue("");
    expect(screen.getByRole("textbox", { name: "搜索记忆" })).toHaveFocus();

    await user.click(screen.getByRole("tab", { name: /会话摘要/ }));
    await user.type(screen.getByRole("textbox", { name: "搜索记忆" }), "不存在的关键词");
    expect(screen.getByRole("status")).toHaveTextContent("没有找到匹配的会话摘要");
  });

  it("没有长期记忆时不会把搜索反馈误写成无匹配结果", async () => {
    api.memoryList.mockResolvedValueOnce([]);
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" />);
    await screen.findByText(/暂无长期记忆文档/);

    await user.type(screen.getByRole("textbox", { name: "搜索记忆" }), "关键词");
    expect(screen.getByRole("status")).toHaveTextContent("暂无长期记忆文档");
    expect(screen.getByRole("status")).not.toHaveTextContent("没有匹配");
    await user.click(screen.getByRole("button", { name: "清除搜索" }));
    expect(screen.getByRole("textbox", { name: "搜索记忆" })).toHaveFocus();
  });

  it("当前分类没有摘要时说明数据为空，即使长期记忆已有内容", async () => {
    api.memoryList.mockResolvedValueOnce([GLOBAL_ENTRY]);
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" />);
    await screen.findByText("MEMORY.md");
    await user.click(screen.getByRole("tab", { name: /会话摘要/ }));

    await user.type(screen.getByRole("textbox", { name: "搜索记忆" }), "关键词");
    expect(screen.getByRole("status")).toHaveTextContent("暂无会话摘要");
    expect(screen.getByRole("status")).not.toHaveTextContent("没有匹配");
  });

  it("新建记忆追加到工作区主文件", async () => {
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" />);
    await screen.findByText("MEMORY.md");

    await user.click(screen.getByRole("button", { name: /追加记忆到文档/ }));
    await user.type(screen.getByPlaceholderText(/TypeScript/), "优先写测试");
    await user.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() => {
      expect(api.memoryAppend).toHaveBeenCalledWith("workspace", "优先写测试", "/repo");
    });
  });

  it("文件已保存但索引更新失败时刷新列表并准确提示", async () => {
    const user = userEvent.setup();
    const onToast = vi.fn();
    api.memorySave.mockRejectedValueOnce(new Error("记忆文件已保存，但检索索引更新失败：数据库正忙"));
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" onToast={onToast} />);
    await screen.findByText("MEMORY.md");

    await user.click(screen.getByTitle("编辑"));
    await user.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(api.memoryList).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("dialog", { name: /编辑 MEMORY.md/ })).not.toBeInTheDocument();
    expect(onToast).toHaveBeenCalledWith("记忆文件已保存，但检索索引更新失败：数据库正忙");
  });

  it("AI 整理只更新草稿，保存时带修订号", async () => {
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" />);
    await screen.findByText("MEMORY.md");

    await user.click(screen.getByTitle("编辑"));
    await user.click(screen.getByRole("button", { name: "AI 整理" }));
    await waitFor(() => {
      expect(api.memoryRewrite).toHaveBeenCalledWith(
        "session-1",
        "# Global\n\nOriginal",
        "全局记忆 MEMORY.md",
      );
    });
    const editor = document.querySelector(".memory-editor__content") as HTMLTextAreaElement;
    await waitFor(() => expect(editor.value).toBe("# Global\n\nRewritten"));
    expect(api.memorySave).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => {
      expect(api.memorySave).toHaveBeenCalledWith(
        "global",
        "MEMORY.md",
        "# Global\n\nRewritten",
        "/repo",
        "rev-global",
      );
    });
  });

  it("单独删除会话摘要", async () => {
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" />);
    await screen.findByText("MEMORY.md");
    await user.click(screen.getByRole("tab", { name: /会话摘要/ }));
    await user.click(screen.getByTitle("删除摘要"));
    await user.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "删除摘要" }));

    await waitFor(() => {
      expect(api.memoryDelete).toHaveBeenCalledWith(
        "session",
        "2026-08-31-session.md",
        "/repo",
        "rev-session",
      );
    });
  });

  it("可一次清空工作区摘要与归档", async () => {
    const user = userEvent.setup();
    render(<ResourcesPanel cwd="/repo" sessionId="session-1" />);
    await screen.findByText("MEMORY.md");
    await user.click(screen.getByRole("tab", { name: /会话摘要/ }));
    await user.click(screen.getByRole("button", { name: "清空摘要" }));
    await user.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "清空摘要" }));

    await waitFor(() => {
      expect(api.memoryClearSessionSummaries).toHaveBeenCalledWith("/repo");
    });
  });
});
