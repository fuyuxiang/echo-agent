import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const listDir = vi.fn();
vi.mock("@/lib/agent-client", () => ({
  listDir: (path: string) => listDir(path),
}));
vi.mock("@/lib/use-element-size", () => ({
  useElementSize: () => 260,
}));

import { FileTreeView } from "@/components/workspace-panel/FileTreeView";

interface Entry {
  name: string;
  path: string;
  kind: "file" | "directory";
  size: number;
  modifiedAt: number;
}

function file(parent: string, name: string): Entry {
  return { name, path: `${parent}/${name}`, kind: "file", size: 1, modifiedAt: 1 };
}

function dir(parent: string, name: string): Entry {
  return { name, path: `${parent}/${name}`, kind: "directory", size: 0, modifiedAt: 1 };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function tree(overrides: Partial<Parameters<typeof FileTreeView>[0]> = {}) {
  return (
    <FileTreeView
      rootPath="/repo"
      onFileSelect={vi.fn()}
      {...overrides}
    />
  );
}

describe("FileTreeView live refresh", () => {
  beforeEach(() => listDir.mockReset());

  it("后台刷新时保留当前文件，不闪回空树", async () => {
    const refresh = deferred<Entry[]>();
    listDir
      .mockResolvedValueOnce([file("/repo", "a.ts")])
      .mockReturnValueOnce(refresh.promise);
    const view = render(tree({ refreshKey: 0 }));
    expect(await screen.findByRole("treeitem", { name: /a\.ts/ })).toBeInTheDocument();

    view.rerender(tree({ refreshKey: 1, refreshPaths: ["b.ts"] }));
    expect(screen.getByRole("treeitem", { name: /a\.ts/ })).toBeInTheDocument();
    expect(screen.queryByText("加载文件树中…")).not.toBeInTheDocument();
    await waitFor(() => expect(listDir).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("treeitem", { name: /a\.ts/ })).toBeInTheDocument();

    await act(async () => refresh.resolve([file("/repo", "a.ts"), file("/repo", "b.ts")]));
    expect(await screen.findByRole("treeitem", { name: /b\.ts/ })).toBeInTheDocument();
  });

  it("只刷新变更所在的已加载目录，并保留展开状态", async () => {
    const refresh = deferred<Entry[]>();
    listDir.mockImplementation((path: string) => {
      if (path === "/repo") return Promise.resolve([dir("/repo", "src")]);
      if (listDir.mock.calls.filter(([called]) => called === "/repo/src").length === 1) {
        return Promise.resolve([file("/repo/src", "a.ts")]);
      }
      return refresh.promise;
    });
    const user = userEvent.setup();
    const view = render(tree({ refreshKey: 0 }));
    const source = await screen.findByRole("treeitem", { name: /src/ });
    await user.click(source);
    expect(await screen.findByRole("treeitem", { name: /a\.ts/ })).toBeInTheDocument();

    view.rerender(tree({ refreshKey: 1, refreshPaths: ["src/b.ts"] }));
    await waitFor(() => expect(listDir).toHaveBeenCalledTimes(3));
    expect(source).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("treeitem", { name: /a\.ts/ })).toBeInTheDocument();
    expect(listDir.mock.calls.filter(([path]) => path === "/repo")).toHaveLength(1);

    await act(async () => refresh.resolve([
      file("/repo/src", "a.ts"),
      file("/repo/src", "b.ts"),
    ]));
    expect(await screen.findByRole("treeitem", { name: /b\.ts/ })).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: /src/ })).toHaveAttribute("aria-expanded", "true");
  });

  it("并发刷新只接受最新响应，迟到数据不会回滚文件树", async () => {
    const older = deferred<Entry[]>();
    const latest = deferred<Entry[]>();
    listDir
      .mockResolvedValueOnce([file("/repo", "base.ts")])
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(latest.promise);
    const view = render(tree({ refreshKey: 0 }));
    expect(await screen.findByRole("treeitem", { name: /base\.ts/ })).toBeInTheDocument();

    view.rerender(tree({ refreshKey: 1, refreshPaths: ["older.ts"] }));
    await waitFor(() => expect(listDir).toHaveBeenCalledTimes(2));
    view.rerender(tree({ refreshKey: 2, refreshPaths: ["latest.ts"] }));
    await waitFor(() => expect(listDir).toHaveBeenCalledTimes(3));
    await act(async () => latest.resolve([file("/repo", "latest.ts")]));
    expect(await screen.findByRole("treeitem", { name: /latest\.ts/ })).toBeInTheDocument();

    await act(async () => older.resolve([file("/repo", "older.ts")]));
    expect(screen.getByRole("treeitem", { name: /latest\.ts/ })).toBeInTheDocument();
    expect(screen.queryByRole("treeitem", { name: /older\.ts/ })).not.toBeInTheDocument();
  });

  it("工作区切换后忽略旧根目录的迟到响应", async () => {
    const oldRoot = deferred<Entry[]>();
    listDir.mockImplementation((path: string) => path === "/old"
      ? oldRoot.promise
      : Promise.resolve([file("/new", "new.ts")]));
    const view = render(tree({ rootPath: "/old" }));
    await waitFor(() => expect(listDir).toHaveBeenCalledWith("/old"));
    view.rerender(tree({ rootPath: "/new" }));
    expect(await screen.findByRole("treeitem", { name: /new\.ts/ })).toBeInTheDocument();

    await act(async () => oldRoot.resolve([file("/old", "old.ts")]));
    expect(screen.queryByRole("treeitem", { name: /old\.ts/ })).not.toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: /new\.ts/ })).toBeInTheDocument();
  });

  it("刷新失败时保留上次成功数据，首次加载失败可重试", async () => {
    const onToast = vi.fn();
    listDir
      .mockResolvedValueOnce([file("/repo", "safe.ts")])
      .mockRejectedValueOnce(new Error("temporary failure"));
    const view = render(tree({ refreshKey: 0, onToast }));
    expect(await screen.findByRole("treeitem", { name: /safe\.ts/ })).toBeInTheDocument();
    view.rerender(tree({ refreshKey: 1, refreshPaths: ["safe.ts"], onToast }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("读取目录失败：temporary failure"));
    expect(screen.getByRole("treeitem", { name: /safe\.ts/ })).toBeInTheDocument();

    listDir
      .mockReset()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce([file("/other", "recovered.ts")]);
    view.rerender(tree({ rootPath: "/other", onToast }));
    const retry = await screen.findByRole("button", { name: "重试" });
    await userEvent.click(retry);
    expect(await screen.findByRole("treeitem", { name: /recovered\.ts/ })).toBeInTheDocument();
  });
});

describe("FileTreeView SP1 — context menu, multi-select, inline rename", () => {
  beforeEach(() => listDir.mockReset());

  it("右键节点触发 onContextMenu 回调", async () => {
    listDir.mockResolvedValue([file("/repo", "a.ts")]);
    const onContextMenu = vi.fn();
    const user = userEvent.setup();
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        onContextMenu={onContextMenu}
      />,
    );
    const node = await screen.findByRole("treeitem", { name: /a\.ts/ });
    await user.pointer({ target: node, keys: "[MouseRight]" });
    expect(onContextMenu).toHaveBeenCalledTimes(1);
    const [, entry] = onContextMenu.mock.calls[0];
    expect(entry.path).toBe("/repo/a.ts");
  });

  it("Cmd/Ctrl+Click 切换多选", async () => {
    listDir.mockResolvedValue([file("/repo", "a.ts"), file("/repo", "b.ts")]);
    const user = userEvent.setup();
    render(<FileTreeView rootPath="/repo" onFileSelect={vi.fn()} />);
    const a = await screen.findByRole("treeitem", { name: /a\.ts/ });
    const b = await screen.findByRole("treeitem", { name: /b\.ts/ });
    await user.click(a);
    expect(a).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{Control>}");
    await user.click(b);
    await user.keyboard("{/Control}");
    expect(a).toHaveAttribute("aria-selected", "true");
    expect(b).toHaveAttribute("aria-selected", "true");
  });

  it("renamingPath 命中时节点显示内联重命名输入框，Enter 提交", async () => {
    listDir.mockResolvedValue([file("/repo", "old.ts")]);
    const onRenameSubmit = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        renamingPath="/repo/old.ts"
        onRenameSubmit={onRenameSubmit}
        onRenameCancel={vi.fn()}
      />,
    );
    const input = await screen.findByTestId("inline-rename-input");
    await user.clear(input);
    await user.type(input, "new.ts");
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(onRenameSubmit).toHaveBeenCalledWith("/repo/old.ts", "new.ts"),
    );
  });

  it("内联重命名抛错时仍保持编辑态", async () => {
    listDir.mockResolvedValue([file("/repo", "old.ts")]);
    const onRenameSubmit = vi.fn().mockRejectedValue(new Error("duplicate"));
    const onRenameCancel = vi.fn();
    const user = userEvent.setup();
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        renamingPath="/repo/old.ts"
        onRenameSubmit={onRenameSubmit}
        onRenameCancel={onRenameCancel}
      />,
    );
    const input = await screen.findByTestId("inline-rename-input");
    await user.clear(input);
    await user.type(input, "new.ts");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(onRenameSubmit).toHaveBeenCalled());
    // still in editing state
    expect(screen.queryByTestId("inline-rename-input")).toBeInTheDocument();
    expect(onRenameCancel).not.toHaveBeenCalled();
  });

  it("Escape 触发 onRenameCancel", async () => {
    listDir.mockResolvedValue([file("/repo", "old.ts")]);
    const onRenameCancel = vi.fn();
    const user = userEvent.setup();
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        renamingPath="/repo/old.ts"
        onRenameSubmit={vi.fn()}
        onRenameCancel={onRenameCancel}
      />,
    );
    const input = await screen.findByTestId("inline-rename-input");
    await user.click(input);
    await user.keyboard("{Escape}");
    expect(onRenameCancel).toHaveBeenCalled();
  });

  it("cutPaths 命中的节点带 file-tree__node--cut class 与 data-cut", async () => {
    listDir.mockResolvedValue([file("/repo", "a.ts")]);
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        cutPaths={new Set(["/repo/a.ts"])}
      />,
    );
    const node = await screen.findByRole("treeitem", { name: /a\.ts/ });
    expect(node).toHaveAttribute("data-cut", "true");
    expect(node.className).toContain("file-tree__node--cut");
  });
});

describe("FileTreeView virtual rows", () => {
  beforeEach(() => listDir.mockReset());

  it("把展开后的子节点作为独立虚拟行渲染", async () => {
    listDir.mockImplementation((path: string) => {
      if (path === "/repo") {
        return Promise.resolve([dir("/repo", "src"), file("/repo", "root.ts")]);
      }
      return Promise.resolve([file("/repo/src", "nested.ts")]);
    });
    const user = userEvent.setup();
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        topLevelThreshold={1}
      />,
    );

    await user.click(await screen.findByRole("treeitem", { name: /src/ }));
    const nested = await screen.findByRole("treeitem", { name: /nested\.ts/ });
    expect(nested).toHaveStyle({ paddingInlineStart: "22px" });
    expect(document.querySelector("[data-fixed-size-list]")).toBeInTheDocument();
  });

  it("虚拟列表中的目录加载失败时仍提供可见重试入口", async () => {
    listDir
      .mockResolvedValueOnce([dir("/repo", "src")])
      .mockRejectedValueOnce(new Error("permission denied"));
    const onToast = vi.fn();
    const user = userEvent.setup();
    render(
      <FileTreeView
        rootPath="/repo"
        onFileSelect={vi.fn()}
        onToast={onToast}
        topLevelThreshold={1}
      />,
    );

    await user.click(await screen.findByRole("treeitem", { name: /src/ }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("读取目录失败：permission denied"));
    const retry = await screen.findByRole("button", { name: "加载失败，重试" });
    expect(retry).toHaveAttribute("title", "permission denied");
  });
});

describe("FileTreeView explorer navigation", () => {
  beforeEach(() => listDir.mockReset());

  it("重新进入项目时恢复已展开目录及其内容", async () => {
    listDir.mockImplementation((path: string) => Promise.resolve(
      path === "/repo"
        ? [dir("/repo", "src")]
        : [file("/repo/src", "restored.ts")],
    ));
    render(tree({ initialExpandedPaths: ["/repo/src"] }));

    expect(await screen.findByRole("treeitem", { name: /restored\.ts/ })).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: /src/ })).toHaveAttribute("aria-expanded", "true");
  });

  it("可一键折叠所有目录", async () => {
    listDir.mockImplementation((path: string) => Promise.resolve(
      path === "/repo" ? [dir("/repo", "src")] : [file("/repo/src", "a.ts")],
    ));
    const user = userEvent.setup();
    const view = render(tree({ collapseKey: 0 }));
    await user.click(await screen.findByRole("treeitem", { name: /src/ }));
    expect(await screen.findByRole("treeitem", { name: /a\.ts/ })).toBeInTheDocument();

    view.rerender(tree({ collapseKey: 1 }));
    expect(screen.getByRole("treeitem", { name: /src/ })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("treeitem", { name: /a\.ts/ })).not.toBeInTheDocument();
  });

  it("切换隐藏文件时原地刷新已加载目录并保留展开状态", async () => {
    listDir.mockImplementation((path: string) => Promise.resolve(
      path === "/repo" ? [dir("/repo", "src")] : [file("/repo/src", "visible.ts")],
    ));
    const user = userEvent.setup();
    const view = render(tree({ includeHidden: false }));
    const source = await screen.findByRole("treeitem", { name: /src/ });
    await user.click(source);
    expect(await screen.findByRole("treeitem", { name: /visible\.ts/ })).toBeInTheDocument();

    view.rerender(tree({ includeHidden: true }));
    await waitFor(() => {
      expect(listDir.mock.calls.filter(([path]) => path === "/repo/src")).toHaveLength(2);
    });
    expect(screen.getByRole("treeitem", { name: /src/ })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("treeitem", { name: /visible\.ts/ })).toBeInTheDocument();
  });

  it("定位文件时自动展开父目录", async () => {
    listDir.mockImplementation((path: string) => Promise.resolve(
      path === "/repo" ? [dir("/repo", "src")] : [file("/repo/src", "target.ts")],
    ));
    render(tree({ revealPath: "/repo/src/target.ts", revealKey: 1 }));

    expect(await screen.findByRole("treeitem", { name: /target\.ts/ })).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: /src/ })).toHaveAttribute("aria-expanded", "true");
  });

  it("支持用方向键展开目录并移动焦点", async () => {
    listDir.mockImplementation((path: string) => Promise.resolve(
      path === "/repo" ? [dir("/repo", "src")] : [file("/repo/src", "keyboard.ts")],
    ));
    const user = userEvent.setup();
    render(tree());
    const source = await screen.findByRole("treeitem", { name: /src/ });
    source.focus();
    await user.keyboard("{ArrowRight}");
    const child = await screen.findByRole("treeitem", { name: /keyboard\.ts/ });
    expect(source).toHaveAttribute("aria-expanded", "true");
    await user.keyboard("{ArrowRight}");
    expect(child).toHaveFocus();
  });
});
