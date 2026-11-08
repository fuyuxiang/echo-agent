// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import type { PersonalMemoryRecord } from "@/lib/agent-client";

const api = vi.hoisted(() => ({ memoryRecordsGet: vi.fn(), memoryRecordSave: vi.fn(), memoryRecordRetire: vi.fn(), memoryFlush: vi.fn() }));
vi.mock("@/lib/agent-client", () => api);
vi.mock("../ResourcesPanel", async () => {
  const { useState } = await import("react");
  const { useSettingsDraft } = await import("@/lib/settings-drafts");
  return { ResourcesPanel: () => {
    const [dirty, setDirty] = useState(false);
    useSettingsDraft({ dirty, label: "原有记忆文档" });
    return <div>原有文档内容<button onClick={() => setDirty(true)}>编辑原有文档</button></div>;
  } };
});
vi.mock("../SettingsSections", () => ({ MemorySettingsPanel: () => <div>对话记忆开关</div> }));
vi.mock("../LocalRetrievalModels", () => ({ LocalRetrievalModels: () => <div>本地模型维护</div> }));
import { PersonalMemoryPanel } from "../PersonalMemoryPanel";

const record: PersonalMemoryRecord = { id: "r1", scope: "global", kind: "user", key: "报告语言", content: "报告使用中文", status: "active", pinned: true, origin: "manual", confidence: 1, sources: ["source-1"], evidence: "请用中文", createdAt: 1, updatedAt: 2, validUntil: null, revision: 3 };
let saved: PersonalMemoryRecord[];
beforeEach(() => {
  vi.clearAllMocks(); saved = [{ ...record }];
  api.memoryRecordsGet.mockImplementation(async () => ({ records: saved, enabled: true, pendingCount: 0, extraction: { outcome: "saved", detail: "", succeededAt: 2 }, retrieval: { at: 0, sources: [] } }));
  api.memoryRecordSave.mockImplementation(async input => { const value = { ...record, ...input, revision: 4 }; saved = [value]; return value; });
  api.memoryRecordRetire.mockImplementation(async (_record, _cwd, completed) => { saved = saved.map(value => ({ ...value, status: completed ? "completed" : "forgotten" })); });
});
function openManager() { fireEvent.click(screen.getByRole("button", { name: "管理" })); }
function openAction(name: string) {
  fireEvent.click(screen.getByRole("button", { name: "更多" }));
  fireEvent.click(screen.getByRole("menuitem", { name }));
}
async function renderManager(cwd?: string) {
  const view = render(<PersonalMemoryPanel cwd={cwd} />); openManager();
  await screen.findByText(record.content);
  return view;
}

describe("记忆设置与二级管理", () => {
  it("编辑旧 /remember 的 80 字标题时不会保留过时事实", async () => {
    const original = "周报使用中文并先给结论。".repeat(10);
    const key = original.slice(0, 80);
    saved = [{ ...record, key, content: original }];
    render(<PersonalMemoryPanel cwd="/project-a" />); openManager();
    fireEvent.click(await screen.findByRole("button", { name: `编辑 ${key}` }));
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: "周报使用双语" } });
    fireEvent.click(screen.getByRole("button", { name: "保存记忆" }));
    await waitFor(() => expect(api.memoryRecordSave).toHaveBeenCalledWith(expect.objectContaining({ key: "周报使用双语", content: "周报使用双语" }), "/project-a"));
  });

  it.each(["用户的报告专属口令为 MEMORY-OLD。", "用户的报告专属口令为 MEMORY-OLD", "用户 的 报告专属口令为 memory old！"])("编辑派生标题记忆时更新标题：%s", async key => {
    const original = "用户的报告专属口令为 MEMORY-OLD。";
    const edited = "用户的报告专属口令为 MEMORY-NEW。";
    saved = [{ ...record, key, content: original, origin: "automatic", pinned: false }];
    render(<PersonalMemoryPanel cwd="/project-a" />); openManager();
    fireEvent.click(await screen.findByRole("button", { name: `编辑 ${key}` }));
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: edited } });
    fireEvent.click(screen.getByRole("button", { name: "保存记忆" }));
    await waitFor(() => expect(api.memoryRecordSave).toHaveBeenCalledWith(expect.objectContaining({ id: record.id, key: edited, content: edited, expectedRevision: record.revision }), "/project-a"));
  });

  it("首页只提供开关和管理入口，列表、排查及旧文档按需加载", async () => {
    render(<PersonalMemoryPanel cwd="/project-a" />);
    await waitFor(() => expect(api.memoryRecordsGet).toHaveBeenCalledWith("/project-a"));
    expect(screen.getByRole("heading", { name: "记忆" })).toBeInTheDocument();
    expect(screen.getByText("对话记忆开关")).toBeInTheDocument();
    expect(screen.queryByText(record.content)).toBeNull();
    expect(screen.queryByText("本地模型维护")).toBeNull();
    expect(screen.queryByText("原有文档内容")).toBeNull();
    expect(screen.queryByRole("button", { name: "添加记忆" })).toBeNull();
    expect(api.memoryRecordSave).not.toHaveBeenCalled();
    openManager();
    expect(await screen.findByText(record.content)).toBeInTheDocument();
    expect(screen.queryByLabelText("类型")).toBeNull();
    expect(screen.queryByText("已固定")).toBeNull();
    expect(screen.queryByRole("tab")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "更多" }));
    expect(screen.getByRole("menu", { name: "记忆操作" })).toHaveStyle({ zIndex: "var(--echo-layer-dialog-popover)" });
    fireEvent.click(screen.getByRole("button", { name: "更多" }));
    openAction("原有记忆文档");
    expect(screen.getByText("原有文档内容")).toBeInTheDocument();
  });

  it("编辑只改正文，保留范围、类型、有效期、优先级和并发版本", async () => {
    saved = [{ ...record, scope: "workspace", kind: "task", pinned: false, validUntil: 4102444800 }];
    const view = await renderManager("/project-a");
    fireEvent.click(screen.getByRole("button", { name: "编辑 报告语言" }));
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: "报告使用双语" } });
    view.rerender(<PersonalMemoryPanel cwd="/project-b" />);
    fireEvent.click(screen.getByRole("button", { name: "保存记忆" }));
    await waitFor(() => expect(api.memoryRecordSave).toHaveBeenCalledWith({ id: "r1", scope: "workspace", kind: "task", key: "报告语言", content: "报告使用双语", pinned: false, validUntil: 4102444800, evidence: "请用中文", expectedRevision: 3 }, "/project-a"));
  });

  it("返回时保护修改，取消返回保留草稿，确认后才离开", async () => {
    const onUnsavedChange = vi.fn();
    render(<PersonalMemoryPanel onUnsavedChange={onUnsavedChange} />); openManager();
    fireEvent.click(await screen.findByRole("button", { name: "编辑 报告语言" }));
    expect(onUnsavedChange).toHaveBeenLastCalledWith(false);
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: "先给结论" } });
    expect(onUnsavedChange).toHaveBeenLastCalledWith(true);
    fireEvent.click(screen.getByRole("button", { name: "返回记忆设置" }));
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(screen.getByLabelText("记忆内容")).toHaveValue("先给结论");
    fireEvent.click(screen.getByRole("button", { name: "返回记忆设置" }));
    fireEvent.click(screen.getByRole("button", { name: "舍弃修改" }));
    expect(screen.getByRole("button", { name: "管理" })).toBeInTheDocument();
    expect(api.memoryRecordSave).not.toHaveBeenCalled();
  });

  it("恢复原值不产生离开警告，保存失败仍保留编辑内容", async () => {
    await renderManager();
    fireEvent.click(screen.getByRole("button", { name: "编辑 报告语言" }));
    const content = screen.getByLabelText("记忆内容");
    fireEvent.change(content, { target: { value: "先给结论" } });
    fireEvent.change(content, { target: { value: record.content } });
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "编辑 报告语言" }));
    fireEvent.change(screen.getByLabelText("记忆内容"), { target: { value: "保存失败时保留我" } });
    api.memoryRecordSave.mockRejectedValueOnce(new Error("磁盘空间不足"));
    fireEvent.click(screen.getByRole("button", { name: "保存记忆" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("磁盘空间不足");
    expect(screen.getByLabelText("记忆内容")).toHaveValue("保存失败时保留我");
  });

  it("忘记后离开当前列表，来源仍可核对，停用记录从更多查看", async () => {
    const user = userEvent.setup(); const open = vi.fn();
    render(<PersonalMemoryPanel cwd="/project-a" onOpenSession={open} />); openManager();
    await user.click(await screen.findByText("查看来源"));
    await user.click(screen.getByRole("button", { name: "打开来源对话 source-1" }));
    expect(open).toHaveBeenCalledWith("source-1");
    await user.click(screen.getByRole("button", { name: "忘记 报告语言" }));
    await user.click(await screen.findByRole("button", { name: "忘记" }));
    await waitFor(() => expect(api.memoryRecordRetire).toHaveBeenCalledWith(expect.objectContaining({ id: "r1", revision: 3 }), "/project-a", false));
    await waitFor(() => expect(screen.queryByText(record.content)).toBeNull());
    openAction("历史记录");
    expect(screen.getByText("已忘记")).toBeInTheDocument();
    expect(screen.getByText(record.content)).toBeInTheDocument();
  });

  it("临时事项在卡片上标明待跟进，不展开来源即可完成并查看历史", async () => {
    saved = [{ ...record, kind: "task" }];
    await renderManager("/project-a");
    expect(screen.getByText("待跟进")).toBeVisible();
    expect(screen.getByText("查看来源").closest("details")).not.toHaveAttribute("open");
    fireEvent.click(screen.getByRole("button", { name: "完成 报告语言" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "将这件事标记为已完成？" })).getByRole("button", { name: "标记完成" }));
    await waitFor(() => expect(api.memoryRecordRetire).toHaveBeenCalledWith(expect.objectContaining({ kind: "task" }), "/project-a", true));
    await waitFor(() => expect(screen.queryByText(record.content)).toBeNull());
    openAction("历史记录");
    expect(screen.getByText("已完成")).toBeVisible();
    expect(screen.queryByText("待跟进")).toBeNull();
    expect(screen.queryByRole("button", { name: "完成 报告语言" })).toBeNull();
  });

  it("导入必须先预览，部分失败保留剩余草稿和原工作区", async () => {
    const view = await renderManager("/project-a"); openAction("导入记忆");
    fireEvent.change(screen.getByLabelText("待导入的记忆"), { target: { value: "回答先给结论\n周报用中文" } });
    expect(api.memoryRecordSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "预览导入" }));
    view.rerender(<PersonalMemoryPanel cwd="/project-b" />);
    api.memoryRecordSave.mockResolvedValueOnce(record).mockRejectedValueOnce(new Error("磁盘空间不足"));
    fireEvent.click(screen.getByRole("button", { name: "添加 2 条记忆" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("已导入 1 条");
    expect(screen.getByRole("button", { name: "添加 1 条记忆" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("第 1 条 · 个人"), { target: { value: "周报用双语" } });
    fireEvent.click(screen.getByRole("button", { name: "添加 1 条记忆" }));
    await waitFor(() => expect(api.memoryRecordSave).toHaveBeenCalledTimes(3));
    expect(api.memoryRecordSave).toHaveBeenLastCalledWith(expect.objectContaining({ key: "周报用双语", content: "周报用双语" }), "/project-a");
    expect(api.memoryRecordSave.mock.calls.every(call => call[1] === "/project-a")).toBe(true);
  });

  it("导入原文变更后必须重新预览，取消返回不丢失导入内容", async () => {
    await renderManager(); openAction("导入记忆");
    const content = screen.getByLabelText("待导入的记忆");
    fireEvent.change(content, { target: { value: "报告先给结论" } });
    fireEvent.click(screen.getByRole("button", { name: "预览导入" }));
    expect(screen.getByRole("button", { name: "添加 1 条记忆" })).toBeInTheDocument();
    fireEvent.change(content, { target: { value: "报告使用双语" } });
    expect(screen.queryByRole("button", { name: "添加 1 条记忆" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "返回记忆设置" }));
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(content).toHaveValue("报告使用双语");
    expect(api.memoryRecordSave).not.toHaveBeenCalled();
  });

  it.each([
    [undefined, "周报使用双语"],
    ["周报先给结论", "周报先给结论"],
  ])("导入预览多次编辑后同步派生标题并保留自定义标题：%s", async (key, expectedKey) => {
    await renderManager("/project-a"); openAction("导入记忆");
    fireEvent.change(screen.getByLabelText("待导入的记忆"), { target: { value: JSON.stringify([{ content: "周报使用中文", key }]) } });
    fireEvent.click(screen.getByRole("button", { name: "预览导入" }));
    const preview = screen.getByLabelText("第 1 条 · 个人");
    // Clearing and then retyping must not turn an automatic title into a custom one.
    fireEvent.change(preview, { target: { value: "" } });
    expect(screen.getByRole("button", { name: "添加 1 条记忆" })).toBeDisabled();
    fireEvent.change(preview, { target: { value: "周报先给结论" } });
    fireEvent.change(preview, { target: { value: "周报使用双语" } });
    fireEvent.click(screen.getByRole("button", { name: "添加 1 条记忆" }));
    await waitFor(() => expect(api.memoryRecordSave).toHaveBeenCalledWith(expect.objectContaining({ key: expectedKey, content: "周报使用双语" }), "/project-a"));
  });

  it("导出沿用有效记录格式，复制失败时仍可手动复制", async () => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error("clipboard unavailable")) } });
    saved.push({ ...record, id: "forgotten", content: "不导出的停用记录", status: "forgotten" });
    await renderManager(); openAction("复制导出");
    const exported = await screen.findByLabelText("导出内容（可复制保存）");
    expect(JSON.parse((exported as HTMLTextAreaElement).value)).toEqual([record]);
  });

  it("原有文档的未保存修改也受到二级返回保护", async () => {
    await renderManager(); openAction("原有记忆文档");
    fireEvent.click(screen.getByRole("button", { name: "编辑原有文档" }));
    fireEvent.click(screen.getByRole("button", { name: "返回管理记忆" }));
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(screen.getByText("原有文档内容")).toBeInTheDocument();
  });

  it("关闭记忆仍可进入管理页，读取失败提供重试且不显示空记忆假象", async () => {
    api.memoryRecordsGet.mockRejectedValueOnce(new Error("读取失败"));
    render(<PersonalMemoryPanel />); openManager();
    expect(await screen.findByRole("alert")).toHaveTextContent("读取失败");
    expect(screen.queryByText("还没有保存的记忆")).toBeNull();
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: false, pendingCount: 0, extraction: {}, retrieval: { at: 0, sources: [] } });
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    expect(await screen.findByText("还没有保存的记忆")).toBeInTheDocument();
    expect(screen.getByText(/开启记忆后/)).toBeInTheDocument();
  });
});

describe("按需访问原有记忆状态", () => {
  it.each([
    [false, false, "待整理内容已保存在本机，开启记忆并恢复自动整理后继续处理"],
    [true, false, "待整理内容已保存在本机，自动整理已暂停，可恢复推荐策略后继续处理"],
    [false, true, "待整理内容已保存在本机，开启记忆后继续处理"],
    [true, true, "待整理内容已保存在本机，将自动继续处理"],
  ])("enabled=%s autoProcessing=%s 仍准确描述已有待整理内容", async (enabled, autoProcessingEnabled, summary) => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled, autoProcessingEnabled, pendingCount: 2, extraction: { outcome: "queued", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel cwd="/project-a" />); openManager(); openAction("记忆状态与排查");
    expect(await screen.findByText(summary as string)).toBeInTheDocument();
    expect(screen.getByText(new RegExp(`2 项内容待整理，${enabled && autoProcessingEnabled ? "打开工作区后自动继续" : "已暂停自动整理"}`))).toBeInTheDocument();
  });

  it("损坏内容保留恢复提示，不混进日常设置", async () => {
    api.memoryRecordsGet.mockResolvedValueOnce({ records: [], enabled: true, pendingCount: 1, quarantinedCount: 2, extraction: { outcome: "queued", succeededAt: null }, retrieval: { at: 0, sources: [] } });
    render(<PersonalMemoryPanel />);
    await waitFor(() => expect(api.memoryRecordsGet).toHaveBeenCalled());
    expect(screen.queryByText(/已隔离 2 项/)).toBeNull();
    openManager(); openAction("记忆状态与排查");
    expect(screen.getByText(/已隔离 2 项损坏的待整理内容/)).toHaveTextContent("损坏部分未完成提取，可打开原对话重新提取");
  });

  it.each([true, false])("手动提取 completed=%s 不会误报已保存新记忆", async completed => {
    api.memoryFlush.mockResolvedValueOnce(completed);
    const onToast = vi.fn();
    render(<PersonalMemoryPanel cwd="/project-a" sessionId="session-a" onToast={onToast} />); openManager();
    await screen.findByText(record.content); openAction("记忆状态与排查");
    fireEvent.click(screen.getByRole("button", { name: "立即提取" }));
    await waitFor(() => expect(api.memoryFlush).toHaveBeenCalledWith("session-a"));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith(completed ? "记忆提取已完成，可在记忆页面查看结果；没有新增内容时不会重复保存" : "本次提取未完成，请查看记忆处理状态后重试"));
  });
});
