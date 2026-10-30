// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import type { PersonalMemoryRecord } from "@/lib/agent-client";

const api = vi.hoisted(() => ({ memoryRecordsGet: vi.fn(), memoryRecordSave: vi.fn(), memoryRecordRetire: vi.fn(), memoryFlush: vi.fn() }));
vi.mock("@/lib/agent-client", () => api);
vi.mock("../ResourcesPanel", () => ({ ResourcesPanel: () => <div>原有记忆文档</div> }));
vi.mock("../SettingsSections", () => ({ MemorySettingsPanel: () => <div>对话记忆开关</div> }));
import { PersonalMemoryPanel } from "../PersonalMemoryPanel";

const record: PersonalMemoryRecord = { id: "r1", scope: "global", kind: "user", key: "报告语言", content: "报告使用中文", status: "active", pinned: true, origin: "manual", confidence: 1, sources: ["source-1"], evidence: "请用中文", createdAt: 1, updatedAt: 2, validUntil: null, revision: 3 };
let saved: PersonalMemoryRecord[];
beforeEach(() => {
  vi.clearAllMocks(); saved = [{ ...record }];
  api.memoryRecordsGet.mockImplementation(async () => ({ records: saved, enabled: true, pendingCount: 0, extraction: { outcome: "saved", detail: "", succeededAt: 2 }, retrieval: { at: 0, sources: [] } }));
  api.memoryRecordSave.mockImplementation(async (input) => { const value = { ...record, ...input, revision: 4 }; saved = [value]; return value; });
  api.memoryRecordRetire.mockImplementation(async () => { saved = saved.map((v) => ({ ...v, status: "forgotten" as const })); });
});

describe("个人记忆管理", () => {
  it("记忆和自动整理都关闭时明确提示两项都需要恢复", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: false, autoProcessingEnabled: false, pendingCount: 1,
      extraction: { outcome: "queued", detail: "", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText("待整理内容已保存在本机，开启记忆并恢复自动整理后继续处理")).toBeInTheDocument();
  });

  it("损坏待办已隔离时说明原件保留和重新提取入口，不误报全部整理完成", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: true, pendingCount: 1, quarantinedCount: 2,
      extraction: { outcome: "queued", detail: "", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText(/已隔离 2 项损坏的待整理内容/)).toHaveTextContent("损坏部分未完成提取，可打开原对话重新提取");
  });
  it("旧版关闭自动整理时不承诺开启记忆后自动处理", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: true, autoProcessingEnabled: false, pendingCount: 1,
      extraction: { outcome: "queued", detail: "", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText("待整理内容已保存在本机，自动整理已暂停，可恢复推荐策略后继续处理")).toBeInTheDocument();
    expect(screen.getByText("1 项内容待整理，已暂停自动整理；不影响已保存的聊天记录")).toBeInTheDocument();
  });

  it("记忆关闭时保留待整理内容，明确说明自动处理已暂停", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: false, pendingCount: 2,
      extraction: { outcome: "queued", detail: "", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText("待整理内容已保存在本机，开启记忆后继续处理")).toBeInTheDocument();
    expect(screen.getByText("2 项内容待整理，已暂停自动整理；不影响已保存的聊天记录")).toBeInTheDocument();
  });
  it("退出后待整理内容明确显示为本地已保存，不误报提取失败", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: true, pendingCount: 2,
      extraction: { outcome: "queued", detail: "", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText("待整理内容已保存在本机，将自动继续处理")).toBeInTheDocument();
    expect(screen.getByText("2 项内容待整理，打开工作区后自动继续；不影响已保存的聊天记录")).toBeInTheDocument();
    expect(screen.queryByText("最近一次提取未完成")).toBeNull();
  });
  it("处理状态明确说明没有新增内容，不把完成时间当成保存凭据", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: true, pendingCount: 0,
      extraction: { outcome: "unchanged", detail: "", succeededAt: 2 }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText("最近一次提取没有新增内容，现有记忆保持不变")).toBeInTheDocument();
    expect(screen.queryByText("最近一次提取已保存记忆")).toBeNull();
  });
  it.each([true, false])("提取结果 completed=%s 不会假定新增记忆或保留了待处理内容", async completed => {
    api.memoryFlush.mockResolvedValueOnce(completed);
    const onToast = vi.fn();
    render(<PersonalMemoryPanel cwd="/project-a" sessionId="session-a" onToast={onToast} />);
    await screen.findByText("报告使用中文");
    fireEvent.click(screen.getByText("记忆状态与排查"));
    fireEvent.click(screen.getByRole("button", { name: "立即提取" }));
    await waitFor(() => expect(api.memoryFlush).toHaveBeenCalledWith("session-a"));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith(completed
      ? "记忆提取已完成，可在记忆页面查看结果；没有新增内容时不会重复保存"
      : "本次提取未完成，请查看记忆处理状态后重试"));
    expect(onToast).not.toHaveBeenCalledWith(expect.stringMatching(/已提取本次对话|已保留待处理/));
    expect(screen.getAllByText("报告使用中文")).toHaveLength(1);
  });
  it("默认只呈现记忆内容，排查和旧文档按需展开", async () => {
    render(<PersonalMemoryPanel cwd="/project-a" />);
    expect(await screen.findByText("报告使用中文")).toBeInTheDocument();
    expect(screen.queryByText("原有记忆文档")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("高级：记忆文档与会话摘要"));
    expect(await screen.findByText("原有记忆文档")).toBeInTheDocument();
  });
  it("更正使用打开草稿时的范围和版本，切换工作区不会改写别的项目", async () => {
    const user = userEvent.setup(); const view = render(<PersonalMemoryPanel cwd="/project-a" />);
    await user.click(await screen.findByRole("button", { name: "更正 报告语言" }));
    await user.clear(screen.getByLabelText("记忆内容")); await user.type(screen.getByLabelText("记忆内容"), "报告使用双语");
    view.rerender(<PersonalMemoryPanel cwd="/project-b" />);
    await user.click(screen.getByRole("button", { name: "保存记忆" }));
    await waitFor(() => expect(api.memoryRecordSave).toHaveBeenCalledWith(expect.objectContaining({ id: "r1", expectedRevision: 3, content: "报告使用双语" }), "/project-a"));
  });
  it("忘记后条目离开有效列表，来源对话能通过专用入口打开", async () => {
    const user = userEvent.setup(); const open = vi.fn(); render(<PersonalMemoryPanel cwd="/project-a" onOpenSession={open} />);
    await user.click(await screen.findByText("来源与依据"));
    await user.click(screen.getByRole("button", { name: "打开来源对话 source-1" })); expect(open).toHaveBeenCalledWith("source-1");
    await user.click(screen.getByRole("button", { name: "忘记 报告语言" }));
    await user.click(await screen.findByRole("button", { name: "忘记" }));
    await waitFor(() => expect(api.memoryRecordRetire).toHaveBeenCalledWith(expect.objectContaining({ id: "r1", revision: 3 }), "/project-a", false));
    await waitFor(() => expect(screen.queryByText("报告使用中文")).not.toBeInTheDocument());
    await user.click(screen.getByRole("tab", { name: "已停用" })); expect(screen.getByText("已忘记")).toBeInTheDocument();
  });
  it("导入必须先预览再保存，失败保留尚未导入的草稿", async () => {
    const user = userEvent.setup(); render(<PersonalMemoryPanel />);
    await user.click(await screen.findByRole("button", { name: "导入记忆" }));
    await user.type(screen.getByLabelText("待导入的记忆"), "回答先给结论\n周报用中文");
    expect(api.memoryRecordSave).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "预览导入" }));
    api.memoryRecordSave.mockResolvedValueOnce(record).mockRejectedValueOnce(new Error("磁盘空间不足"));
    await user.click(screen.getByRole("button", { name: "添加 2 条记忆" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("已导入 1 条");
    expect(screen.getByRole("button", { name: "添加 1 条记忆" })).toBeInTheDocument();
  });
});


it("查看原记忆不算修改，编辑后恢复原值不触发离开警告", async () => {
  const onUnsavedChange = vi.fn();
  render(<PersonalMemoryPanel onUnsavedChange={onUnsavedChange} />);
  fireEvent.click(await screen.findByRole("button", { name: "更正 报告语言" }));
  expect(onUnsavedChange).toHaveBeenLastCalledWith(false);
  const content = screen.getByLabelText("记忆内容");
  fireEvent.change(content, { target: { value: "先给结论" } });
  expect(onUnsavedChange).toHaveBeenLastCalledWith(true);
  fireEvent.change(content, { target: { value: record.content } });
  expect(onUnsavedChange).toHaveBeenLastCalledWith(false);
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(screen.queryByLabelText("记忆内容")).toBeNull();
});

it("导入原文变更后必须重新预览，不能保存旧预览", async () => {
  render(<PersonalMemoryPanel />);
  fireEvent.click(await screen.findByRole("button", { name: "导入记忆" }));
  const content = screen.getByLabelText("待导入的记忆");
  fireEvent.change(content, { target: { value: "报告先给结论" } });
  fireEvent.click(screen.getByRole("button", { name: "预览导入" }));
  expect(screen.getByRole("button", { name: "添加 1 条记忆" })).toBeInTheDocument();
  fireEvent.change(content, { target: { value: "报告使用双语" } });
  expect(screen.queryByRole("button", { name: "添加 1 条记忆" })).toBeNull();
  expect(api.memoryRecordSave).not.toHaveBeenCalled();
});
