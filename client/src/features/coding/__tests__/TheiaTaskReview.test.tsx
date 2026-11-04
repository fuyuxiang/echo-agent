import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TheiaTaskReview, reviewLanguage } from "../TheiaTaskReview";

const { prepareLocalMonaco, editorState } = vi.hoisted(() => ({
  prepareLocalMonaco: vi.fn(async (): Promise<void> => undefined),
  editorState: { fail: false, automaticMount: true, onMount: null as (() => void) | null },
}));
vi.mock("../lib/local-monaco", () => ({ prepareLocalMonaco, syncLocalMonacoTheme: vi.fn() }));

const invoke = vi.fn(async (command: string, _args?: unknown): Promise<unknown> => {
  if (command === "coding_changeset_diff") {
    return { original: "binary", modified: "binary", binary: true, modifiedHash: "content-v1" };
  }
  return null;
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args: unknown) => invoke(command, args) }));
vi.mock("@monaco-editor/react", async () => {
  const React = await import("react");
  return { DiffEditor: ({ onMount }: { onMount?: () => void }) => {
    React.useEffect(() => {
      editorState.onMount = onMount ?? null;
      if (editorState.automaticMount) onMount?.();
    }, []);
    if (editorState.fail) throw new Error("编辑器创建失败");
    return <div>文本差异</div>;
  } };
});

describe("Theia task review", () => {
  it("uses Monaco language IDs for the files reviewed most often", () => {
    expect(reviewLanguage("src/App.tsx")).toBe("typescript");
    expect(reviewLanguage("src-tauri/src/main.rs")).toBe("rust");
    expect(reviewLanguage("tests/test_api.py")).toBe("python");
    expect(reviewLanguage("docs/README.md")).toBe("markdown");
    expect(reviewLanguage("README")).toBe("plaintext");
  });
  beforeEach(() => {
    prepareLocalMonaco.mockReset();
    prepareLocalMonaco.mockResolvedValue(undefined);
    editorState.fail = false;
    editorState.automaticMount = true;
    editorState.onMount = null;
    invoke.mockReset();
    invoke.mockImplementation(async (command: string): Promise<unknown> => {
      if (command === "coding_changeset_diff") {
        return { original: "binary", modified: "binary", binary: true, modifiedHash: "content-v1" };
      }
      return null;
    });
  });

  it("本地编辑器加载完成前不能把尚未显示的文本差异标为已审阅", async () => {
    let resolve!: () => void;
    prepareLocalMonaco.mockImplementation(() => new Promise<void>((done) => { resolve = done; }));
    invoke.mockResolvedValue({ original: "old", modified: "new", binary: false, modifiedHash: "content-v1" });
    render(<TheiaTaskReview root="/repo" taskId="task-1" path="file.ts" onClose={vi.fn()} onOpenFile={vi.fn()} onReviewed={vi.fn()} />);
    await waitFor(() => expect(prepareLocalMonaco).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeDisabled();
    expect(screen.queryByText("文本差异")).not.toBeInTheDocument();
    await act(async () => { resolve(); });
    await waitFor(() => expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled());
    expect(screen.getByText("文本差异")).toBeInTheDocument();
  });

  it("本地编辑器加载失败显示原位重试且保留当前文件与版本", async () => {
    prepareLocalMonaco.mockRejectedValueOnce(new Error("编辑器资源读取失败")).mockResolvedValue(undefined);
    invoke.mockResolvedValue({ original: "old", modified: "new", binary: false, modifiedHash: "content-v1" });
    const onReviewed = vi.fn(async () => undefined);
    render(<TheiaTaskReview root="/repo" taskId="task-1" path="file.ts" onClose={vi.fn()} onOpenFile={vi.fn()} onReviewed={onReviewed} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("编辑器资源读取失败");
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "重试加载差异" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled());
    await userEvent.click(screen.getByRole("button", { name: "标记已审阅" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_changeset_mark_reviewed", {
      root: "/repo", taskId: "task-1", path: "file.ts", expectedHash: "content-v1",
    }));
    expect(onReviewed).toHaveBeenCalledOnce();
  });

  it("切换任务后旧编辑器加载失败不会污染当前差异", async () => {
    let reject!: (reason: Error) => void;
    prepareLocalMonaco.mockImplementationOnce(() => new Promise<void>((_, fail) => { reject = fail; }))
      .mockResolvedValue(undefined);
    invoke.mockResolvedValue({ original: "old", modified: "new", binary: false, modifiedHash: "content-v1" });
    const props = { root: "/repo", onClose: vi.fn(), onOpenFile: vi.fn(), onReviewed: vi.fn(async () => undefined) };
    const view = render(<TheiaTaskReview {...props} taskId="task-1" path="first.ts" />);
    await waitFor(() => expect(prepareLocalMonaco).toHaveBeenCalledOnce());
    view.rerender(<TheiaTaskReview {...props} taskId="task-2" path="second.ts" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled());
    await act(async () => { reject(new Error("旧任务资源读取失败")); });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByText("文本差异")).toBeInTheDocument();
  });

  it("本地资源已准备仍须等待真实编辑器mount才能审阅", async () => {
    editorState.automaticMount = false;
    invoke.mockResolvedValue({ original: "old", modified: "new", binary: false, modifiedHash: "content-v1" });
    render(<TheiaTaskReview root="/repo" taskId="task-1" path="file.ts" onClose={vi.fn()} onOpenFile={vi.fn()} onReviewed={vi.fn()} />);
    await screen.findByText("文本差异");
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeDisabled();
    await act(async () => { editorState.onMount?.(); });
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled();
  });

  it("A到B再返回A不能复用旧mount，迟到的旧A回调也不能批准新差异", async () => {
    editorState.automaticMount = false;
    invoke.mockResolvedValue({ original: "old", modified: "new", binary: false, modifiedHash: "content-v1" });
    const props = { root: "/repo", taskId: "task-1", onClose: vi.fn(), onOpenFile: vi.fn(), onReviewed: vi.fn(async () => undefined) };
    const view = render(<TheiaTaskReview {...props} path="A.ts" />);
    await screen.findByText("文本差异");
    const firstMount = editorState.onMount;
    await act(async () => { firstMount?.(); });
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled();
    view.rerender(<TheiaTaskReview {...props} path="B.ts" />);
    await waitFor(() => expect(editorState.onMount).not.toBe(firstMount));
    const secondMount = editorState.onMount;
    await screen.findByText("文本差异");
    view.rerender(<TheiaTaskReview {...props} path="A.ts" />);
    await waitFor(() => expect(editorState.onMount).not.toBe(secondMount));
    await screen.findByText("文本差异");
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeDisabled();
    const freshMount = editorState.onMount;
    await act(async () => { firstMount?.(); });
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeDisabled();
    await act(async () => { freshMount?.(); });
    expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled();
  });

  it("编辑器创建异常可重试且失败时不能假审阅", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      editorState.fail = true;
      invoke.mockResolvedValue({ original: "old", modified: "new", binary: false, modifiedHash: "content-v1" });
      render(<TheiaTaskReview root="/repo" taskId="task-1" path="file.ts" onClose={vi.fn()} onOpenFile={vi.fn()} onReviewed={vi.fn()} />);
      expect(await screen.findByRole("alert")).toHaveTextContent("编辑器创建失败");
      expect(screen.getByRole("button", { name: "标记已审阅" })).toBeDisabled();
      editorState.fail = false;
      await userEvent.click(screen.getByRole("button", { name: "重试加载差异" }));
      await waitFor(() => expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled());
      expect(consoleError).toHaveBeenCalled();
    } finally { consoleError.mockRestore(); }
  });

  it("requires explicit binary review and binds approval to the displayed content version", async () => {
    const onReviewed = vi.fn(async () => undefined);
    render(<TheiaTaskReview root="/repo" taskId="task-1" path="asset.png" onClose={vi.fn()} onOpenFile={vi.fn()} onReviewed={onReviewed} />);
    const button = await screen.findByRole("button", { name: "标记已审阅" });
    expect(button).toBeDisabled();
    await userEvent.click(screen.getByRole("checkbox", { name: "我已核对当前版本的二进制文件" }));
    expect(button).toBeEnabled();
    await userEvent.click(button);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("coding_changeset_mark_reviewed", {
      root: "/repo", taskId: "task-1", path: "asset.png", expectedHash: "content-v1",
    }));
    expect(onReviewed).toHaveBeenCalledOnce();
  });

  it("切换差异文件后旧审阅失败不会覆盖新文件或禁用其审阅", async () => {
    let reject!: (reason: Error) => void;
    invoke.mockImplementation(async (command: string): Promise<unknown> => {
      if (command === "coding_changeset_mark_reviewed") return new Promise((_resolve, fail) => { reject = fail; });
      return { original: "a", modified: "b", binary: false, modifiedHash: "content-v1" };
    });
    const onReviewed = vi.fn(async () => undefined);
    const props = { root: "/repo", taskId: "task-1", onClose: vi.fn(), onOpenFile: vi.fn(), onReviewed };
    const view = render(<TheiaTaskReview {...props} path="first.ts" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled());
    await userEvent.click(screen.getByRole("button", { name: "标记已审阅" }));
    view.rerender(<TheiaTaskReview {...props} path="second.ts" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "标记已审阅" })).toBeEnabled());
    await act(async () => { reject(new Error("旧文件已改变")); });
    expect(screen.queryByText(/旧文件已改变/)).not.toBeInTheDocument();
    expect(screen.getByText("文本差异")).toBeInTheDocument();
  });
});
