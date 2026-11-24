import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useRef } from "react";

const mocks = vi.hoisted(() => ({
  listDir: vi.fn(async (_path?: string) => [] as unknown[]),
}));

vi.mock("@/lib/agent-client", () => ({
  listDir: mocks.listDir,
}));

import { FileTreeView } from "../FileTreeView";

describe("FileTreeView filter", () => {
  beforeEach(() => {
    mocks.listDir.mockReset();
    mocks.listDir.mockResolvedValue([]);
  });

  it("filters entries by case-insensitive substring", async () => {
    mocks.listDir.mockResolvedValue([
      { name: "README.md", path: "/r/README.md", kind: "file", size: 0, modifiedAt: 0, isLarge: false, isBinary: false },
      { name: "package.json", path: "/r/package.json", kind: "file", size: 0, modifiedAt: 0, isLarge: false, isBinary: false },
    ]);

    render(
      <FileTreeView
        rootPath="/r"
        onFileSelect={() => {}}
        filter="read"
      />,
    );

    expect(await screen.findByText("README.md")).not.toBeNull();
    expect(screen.queryByText("package.json")).toBeNull();
  });

  it("shows all entries when filter is empty", async () => {
    mocks.listDir.mockResolvedValue([
      { name: "a.ts", path: "/r/a.ts", kind: "file", size: 0, modifiedAt: 0, isLarge: false, isBinary: false },
      { name: "b.ts", path: "/r/b.ts", kind: "file", size: 0, modifiedAt: 0, isLarge: false, isBinary: false },
    ]);
    render(
      <FileTreeView rootPath="/r" onFileSelect={() => {}} />,
    );
    expect(await screen.findByText("a.ts")).not.toBeNull();
    expect(screen.getByText("b.ts")).not.toBeNull();
  });

  it("shows cwd-empty hint when root has no entries", async () => {
    mocks.listDir.mockResolvedValue([]);
    const onSelectDirectory = vi.fn();
    render(<FileTreeView rootPath="/empty" onFileSelect={() => {}} onSelectDirectory={onSelectDirectory} />);
    expect(await screen.findByText(/没有可见文件/)).not.toBeNull();
    screen.getByRole("button", { name: "选择其他目录" }).click();
    expect(onSelectDirectory).toHaveBeenCalledTimes(1);
  });

  it("在折叠目录内匹配文件时按需加载并保留祖先层级", async () => {
    mocks.listDir.mockImplementation(async (path?: string) => path === "/r"
      ? [{ name: "src", path: "/r/src", kind: "directory", size: 0, modifiedAt: 0, isLarge: false, isBinary: false }]
      : [{ name: "deep-match.ts", path: "/r/src/deep-match.ts", kind: "file", size: 0, modifiedAt: 0, isLarge: false, isBinary: false }]);
    render(
      <FileTreeView
        rootPath="/r"
        onFileSelect={() => {}}
        filter="deep"
        indexedPaths={["src/deep-match.ts"]}
      />,
    );
    expect(await screen.findByText("deep-match.ts")).toBeInTheDocument();
    expect(screen.getByText("src")).toBeInTheDocument();
    expect(mocks.listDir).toHaveBeenCalledWith("/r/src", "/r", undefined, false);
  });
});

describe("FileTreeView 渲染范围", () => {
  afterEach(() => vi.unstubAllGlobals());

  function measureViewport() {
    const observed = vi.fn();
    vi.stubGlobal("ResizeObserver", class implements ResizeObserver {
      constructor(private readonly update: ResizeObserverCallback) {}
      observe(target: Element) {
        observed(target);
        this.update([{
          target,
          contentRect: new DOMRectReadOnly(0, 0, 320, 260),
          borderBoxSize: [], contentBoxSize: [], devicePixelContentBoxSize: [],
        }], this);
      }
      disconnect() {}
      unobserve() {}
    });
    return observed;
  }

  it("目录异步返回后观察可见容器，固定窗口只渲染有限的行", async () => {
    const rows = Array.from({ length: 500 }, (_, index) => ({
      name: `row-${index}.txt`, path: `/r/row-${index}.txt`, kind: "file", size: 1, modifiedAt: 0,
    }));
    mocks.listDir.mockResolvedValue(rows);
    const observe = measureViewport();
    const { container } = render(<FileTreeView rootPath="/r" onFileSelect={() => {}} />);
    expect(screen.getByText("加载文件树中…")).toBeInTheDocument();
    await waitFor(() => expect(container.querySelector('[data-tree-path="/r/row-0.txt"]')).not.toBeNull(), { timeout: 12_000 });
    const visibleRows = container.querySelectorAll('[data-tree-path]');
    expect(visibleRows.length).toBe(26);
    expect(observe).toHaveBeenCalledWith(screen.getByRole("tree"));
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.queryByText("row-499.txt")).not.toBeInTheDocument();
  }, 15_000);

  it("复用工作区滚动容器，并让键盘定位尚未渲染的目标文件", async () => {
    mocks.listDir.mockResolvedValue(Array.from({ length: 500 }, (_, index) => ({
      name: `row-${index}.txt`, path: `/r/row-${index}.txt`, kind: "file", size: 1, modifiedAt: 0,
    })));
    measureViewport();
    function Workspace() {
      const viewportRef = useRef<HTMLDivElement>(null);
      return <>
        <button type="button">工作区导航</button>
        <div ref={viewportRef} data-testid="viewport">
          <FileTreeView rootPath="/r" selectedPath="/r/row-0.txt" onFileSelect={() => {}} viewportRef={viewportRef} />
        </div>
      </>;
    }
    const { container } = render(<Workspace />);
    const firstRow = await waitFor(() => {
      const row = container.querySelector('[data-tree-path="/r/row-0.txt"]');
      expect(row).not.toBeNull();
      return row!;
    }, { timeout: 12_000 });
    const viewport = screen.getByTestId("viewport");
    act(() => {
      viewport.scrollTop = 2600;
      fireEvent.scroll(viewport);
    });
    expect(await screen.findByText("row-100.txt")).toBeInTheDocument();
    expect(firstRow).not.toBeInTheDocument();
    const user = userEvent.setup();
    screen.getByRole("button", { name: "工作区导航" }).focus();
    await user.tab();
    expect(screen.getByRole("treeitem", { name: "row-100.txt" })).toHaveFocus();
    await user.keyboard("{End}");
    await waitFor(() => expect(screen.getByText("row-499.txt").closest('[role="treeitem"]')).toHaveFocus());
    expect(screen.getByRole("treeitem", { name: "row-499.txt" })).toHaveAttribute("tabindex", "0");
    expect(viewport.scrollTop).toBeGreaterThan(2600);
  }, 15_000);
});
