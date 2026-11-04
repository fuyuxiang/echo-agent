import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { markdownToDocument } from "@/lib/document-model";
import type { DocumentSnapshot, EditableDocument } from "@/lib/document-workspace";

const mocks = vi.hoisted(() => ({ session: vi.fn(), register: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@/lib/document-workspace", () => ({ documentSession: mocks.session }));
vi.mock("@/lib/artifact-catalog", async importOriginal => ({
  ...await importOriginal<typeof import("@/lib/artifact-catalog")>(),
  registerDocumentArtifact: mocks.register,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
// These tests concern host callbacks after navigation, not the editor engine.
vi.mock("@tiptap/react", () => ({ useEditor: () => null, EditorContent: () => null }));

import DocumentWorkbench from "../DocumentWorkbench";
import { ToolSidePanel } from "../ToolSidePanel";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

const document = markdownToDocument("# 原文\n\n需要保留的内容。");
const record: EditableDocument = { id: "copy", sessionId: "task-a", path: "/work/副本.docx", title: "副本",
  revision: 1, exportedRevision: 1, fileSha256: "a".repeat(64), byteSize: 128,
  document, savedAt: 1, actor: "用户", history: [] };
const base = { path: "/work/原文.md", sessionId: "task-a", cwd: "/work", content: "# 原文\n\n需要保留的内容。" };

function sessionFor(current: EditableDocument | null = null) {
  const state: DocumentSnapshot = { opened: { document: current, path: base.path, fileSha256: "b".repeat(64), byteSize: 128 },
    record: current, draft: current?.document ?? null, phase: "ready", message: "", publishing: false, syncError: "", operation: null, failure: null };
  const session = { snapshot: () => state, subscribe: () => () => {}, refresh: vi.fn().mockResolvedValue(undefined),
    copy: vi.fn(), flush: vi.fn(), publish: vi.fn(), export: vi.fn() };
  mocks.session.mockReturnValue(session);
  return session;
}

beforeEach(() => {
  mocks.session.mockReset();
  mocks.register.mockReset().mockReturnValue({ id: "copy", path: record.path });
  mocks.invoke.mockReset().mockImplementation(async (command, args) => {
    if (command === "read_text_file") return base.content;
    if (command === "path_stat") return { absolute: args.path };
    return [];
  });
});

describe("document actions after navigation", () => {
  it("records a completed copy for its owner without reopening it over another task", async () => {
    const pending = deferred<EditableDocument>();
    sessionFor().copy.mockReturnValue(pending.promise);
    const open = vi.fn(), toast = vi.fn();
    const view = render(<DocumentWorkbench {...base} onOpenArtifact={open} onToast={toast} />);
    fireEvent.click(screen.getByRole("button", { name: "创建可编辑副本" }));
    view.unmount();
    render(<DocumentWorkbench {...base} sessionId="task-b" path="/work/任务二.md" onOpenArtifact={open} onToast={toast} />);
    await act(async () => { pending.resolve(record); });
    expect(mocks.register).toHaveBeenCalledWith(expect.objectContaining({ path: record.path }), "task-a", record.title, "/work");
    expect(open).not.toHaveBeenCalled();
    expect(toast).not.toHaveBeenCalled();
  });

  it("does not inject a document request or close the current panel after an old flush", async () => {
    const pending = deferred<void>();
    sessionFor(record).flush.mockReturnValue(pending.promise);
    const ask = vi.fn();
    const view = render(<DocumentWorkbench {...base} onAskAgent={ask} />);
    fireEvent.click(screen.getByRole("button", { name: "让 Agent 修改" }));
    view.unmount();
    await act(async () => { pending.resolve(); });
    expect(ask).not.toHaveBeenCalled();
  });

  it("keeps late save failures in their document state without toasting another view", async () => {
    const pending = deferred<void>();
    sessionFor(record).publish.mockReturnValue(pending.promise);
    const toast = vi.fn();
    const view = render(<DocumentWorkbench {...base} onToast={toast} />);
    fireEvent.click(screen.getByRole("button", { name: "保存到 Word" }));
    view.unmount();
    await act(async () => { pending.reject(new Error("磁盘不可写")); });
    expect(toast).not.toHaveBeenCalled();
  });

  it("still opens a copy and explains success when its initiating view is active", async () => {
    sessionFor().copy.mockResolvedValue(record);
    const open = vi.fn(), toast = vi.fn();
    render(<DocumentWorkbench {...base} onOpenArtifact={open} onToast={toast} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "创建可编辑副本" })); });
    expect(open).toHaveBeenCalledOnce();
    expect(toast).toHaveBeenCalledWith("已创建可编辑副本，原件已保留");
  });

  it.each([false, true])("does not reopen a closed panel, including after reopening it before completion (%s)", async reopen => {
    const pending = deferred<EditableDocument>();
    const session = sessionFor();
    session.copy.mockReturnValue(pending.promise);
    const open = vi.fn(), toast = vi.fn();
    const view = render(<DocumentWorkbench {...base} active onOpenArtifact={open} onToast={toast} />);
    fireEvent.click(screen.getByRole("button", { name: "创建可编辑副本" }));
    view.rerender(<DocumentWorkbench {...base} active={false} onOpenArtifact={open} onToast={toast} />);
    if (reopen) {
      view.rerender(<DocumentWorkbench {...base} active onOpenArtifact={open} onToast={toast} />);
      expect(screen.getByRole("button", { name: "正在创建副本…" })).toBeDisabled();
    }
    await act(async () => { pending.resolve(record); });
    expect(mocks.register).toHaveBeenCalledWith(expect.objectContaining({ path: record.path }), "task-a", record.title, "/work");
    expect(open).not.toHaveBeenCalled();
    expect(toast).not.toHaveBeenCalled();
    view.rerender(<DocumentWorkbench {...base} active onOpenArtifact={open} onToast={toast} />);
    expect(screen.getByRole("button", { name: "创建可编辑副本" })).toBeEnabled();
    session.copy.mockResolvedValue(record);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "创建可编辑副本" })); });
    expect(open).toHaveBeenCalledOnce();
  });

  it("does not inject Agent context after the panel closes during a save", async () => {
    const pending = deferred<void>();
    sessionFor(record).flush.mockReturnValue(pending.promise);
    const ask = vi.fn();
    const view = render(<DocumentWorkbench {...base} active onAskAgent={ask} />);
    fireEvent.click(screen.getByRole("button", { name: "让 Agent 修改" }));
    view.rerender(<DocumentWorkbench {...base} active={false} onAskAgent={ask} />);
    await act(async () => { pending.resolve(); });
    expect(ask).not.toHaveBeenCalled();
    view.rerender(<DocumentWorkbench {...base} active onAskAgent={ask} />);
    expect(screen.getByRole("button", { name: "让 Agent 修改" })).toBeEnabled();
  });

  it("releases the busy state after a hidden action fails without a late toast", async () => {
    const pending = deferred<void>();
    sessionFor(record).publish.mockReturnValue(pending.promise);
    const toast = vi.fn();
    const view = render(<DocumentWorkbench {...base} active onToast={toast} />);
    fireEvent.click(screen.getByRole("button", { name: "保存到 Word" }));
    view.rerender(<DocumentWorkbench {...base} active={false} onToast={toast} />);
    await act(async () => { pending.reject(new Error("磁盘不可写")); });
    view.rerender(<DocumentWorkbench {...base} active onToast={toast} />);
    expect(screen.getByRole("button", { name: "保存到 Word" })).toBeEnabled();
    expect(toast).not.toHaveBeenCalled();
  });

  it("propagates closing the real workspace panel to its still-mounted document", async () => {
    const pending = deferred<EditableDocument>();
    sessionFor().copy.mockReturnValue(pending.promise);
    const open = vi.fn(), toast = vi.fn();
    const props = { open: true, mode: "artifacts" as const, cwd: base.cwd, sessionId: base.sessionId,
      previewPath: base.path, previewRequestId: 1,
      artifacts: [{ id: base.path, path: base.path, kind: "file", title: "原文", toolCallId: "", status: "completed" as const }],
      onClose: vi.fn(), onSelectTool: vi.fn(), onSelectArtifact: open, onOpenArtifacts: vi.fn(), onToast: toast };
    const view = render(<ToolSidePanel {...props} />);
    fireEvent.click(await screen.findByRole("button", { name: "创建可编辑副本" }));
    const documentView = view.container.querySelector(".document-workbench");
    view.rerender(<ToolSidePanel {...props} open={false} />);
    expect(view.container.querySelector(".document-workbench")).toBe(documentView);
    await act(async () => { pending.resolve(record); });
    expect(mocks.register).toHaveBeenCalledOnce();
    expect(open).not.toHaveBeenCalled();
    expect(toast).not.toHaveBeenCalled();
    view.rerender(<ToolSidePanel {...props} />);
    expect(screen.getByRole("button", { name: "创建可编辑副本" })).toBeEnabled();
  });

  it.each(["Word", "PDF"])("keeps a late %s export without navigating or toasting a reopened view", async format => {
    const pending = deferred<{path:string;sha256:string;byteSize:number}>();
    sessionFor(record).export.mockReturnValue(pending.promise);
    const open = vi.fn(), toast = vi.fn();
    const view = render(<DocumentWorkbench {...base} active onOpenArtifact={open} onToast={toast} />);
    fireEvent.click(screen.getByRole("button", { name: "导出文档" }));
    fireEvent.click(screen.getByRole("menuitem", { name: `另存为 ${format}` }));
    view.rerender(<DocumentWorkbench {...base} active={false} onOpenArtifact={open} onToast={toast} />);
    view.rerender(<DocumentWorkbench {...base} active onOpenArtifact={open} onToast={toast} />);
    await act(async () => { pending.resolve({path:"/work/exported.docx",sha256:"c".repeat(64),byteSize:256}); });
    expect(open).not.toHaveBeenCalled();
    expect(toast).not.toHaveBeenCalled();
    expect(mocks.register).toHaveBeenCalledTimes(format === "Word" ? 1 : 0);
    expect(screen.getByRole("button", { name: "保存到 Word" })).toBeEnabled();
  });

  it("suppresses late clipboard feedback after closing the panel", async () => {
    const pending = deferred<void>();
    const previous = Object.getOwnPropertyDescriptor(navigator, "clipboard");
    Object.defineProperty(navigator, "clipboard", { configurable:true, value:{writeText:vi.fn(() => pending.promise)} });
    try {
      sessionFor(record);
      const toast = vi.fn();
      const view = render(<DocumentWorkbench {...base} active onToast={toast} />);
      fireEvent.click(screen.getByRole("button", { name: "复制文本" }));
      view.rerender(<DocumentWorkbench {...base} active={false} onToast={toast} />);
      await act(async () => { pending.resolve(); });
      expect(toast).not.toHaveBeenCalled();
    } finally {
      if (previous) Object.defineProperty(navigator, "clipboard", previous);
      else Reflect.deleteProperty(navigator, "clipboard");
    }
  });

  it("never calls an unsaved draft saved when its previous Word revision was synchronized", () => {
    const session = sessionFor(record);
    Object.assign(session.snapshot(), { phase: "error", failure: "save", message: "编辑稿暂时不可写",
      draft: markdownToDocument("# 尚未保存的新内容") });
    render(<DocumentWorkbench {...base} />);
    expect(screen.getByRole("status")).toHaveTextContent("编辑稿保存失败 · 修改已保留");
    expect(screen.getByRole("button", { name: "重试保存" })).toBeInTheDocument();
  });

  it("retries reading the latest source when a refresh fails after an earlier save", async () => {
    const session = sessionFor(record);
    Object.assign(session.snapshot(), { phase: "error", failure: "read", message: "读取最新稿失败" });
    render(<DocumentWorkbench {...base} />);
    expect(screen.getByRole("status")).toHaveTextContent("读取最新稿失败 · 当前内容已保留");
    session.refresh.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "重试读取" }));
    expect(session.refresh).toHaveBeenCalledOnce();
    expect(session.flush).not.toHaveBeenCalled();
  });
});
