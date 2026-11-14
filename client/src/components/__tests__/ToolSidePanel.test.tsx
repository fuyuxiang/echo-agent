import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { ToolSidePanel, type ToolSidePanelMode } from "../ToolSidePanel";
import { SidebarLayoutContext, useAppSidebarLayout } from "@/lib/sidebar-layout";
import type { SessionArtifact } from "@/lib/session-artifacts";
import type { ChatMessage, ToolCallView } from "@/stores/session-store";
import type { AutomationStatus } from "@/lib/automation-client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function renderPanel(
  mode: ToolSidePanelMode = "artifacts",
  artifacts: SessionArtifact[] = [],
  onToast = vi.fn(),
) {
  return render(
    <ToolSidePanel
      open
      mode={mode}
      artifacts={artifacts}
      cwd="C:\\work"
      messages={[]}
      sessionId="session-1"
      onClose={vi.fn()}
      onSelectTool={vi.fn()}
      onSelectArtifact={vi.fn()}
      onOpenArtifacts={vi.fn()}
      onToast={onToast}
    />,
  );
}

describe("ToolSidePanel resize lifecycle", () => {
  let viewport: number;
  beforeEach(() => {
    viewport = window.innerWidth;
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1920 });
    localStorage.clear();
    vi.mocked(invoke).mockReset().mockResolvedValue([]);
  });
  afterEach(() => Object.defineProperty(window, "innerWidth", { configurable: true, value: viewport }));
  const pointer = (type: string, x: number) => {
    const event = new Event(type, { bubbles: true });
    Object.assign(event, { pointerId: 1, clientX: x, button: 0, buttons: type === "pointerup" ? 0 : 1, isPrimary: true });
    return event;
  };
  it.each(["blur", "Escape", "pointercancel", "unmount"])("%s cancels the preview and removes drag listeners", reason => {
    const view = renderPanel();
    const handle = view.container.querySelector(".tool-side-panel__edge-sash")!;
    fireEvent(handle, pointer("pointerdown", 1000));
    fireEvent(window, pointer("pointermove", 900));
    expect(view.container.querySelector("aside")).toHaveStyle({ width: "520px" });
    if (reason === "unmount") view.unmount();
    else if (reason === "Escape") fireEvent.keyDown(window, { key: "Escape" });
    else fireEvent(window, reason === "blur" ? new Event("blur") : pointer(reason, 900));
    fireEvent(window, pointer("pointermove", 800));
    if (reason !== "unmount") expect(view.container.querySelector("aside")).toHaveStyle({ width: "420px" });
    expect(localStorage.getItem("tool-side-panel-width")).toBe("420");
  });
  it("persists only the final release coordinate", () => {
    const view = renderPanel();
    fireEvent(view.container.querySelector(".tool-side-panel__edge-sash")!, pointer("pointerdown", 1000));
    fireEvent(window, pointer("pointermove", 900));
    expect(localStorage.getItem("tool-side-panel-width")).toBe("420");
    fireEvent(window, pointer("pointerup", 860));
    expect(view.container.querySelector("aside")).toHaveStyle({ width: "560px" });
    expect(localStorage.getItem("tool-side-panel-width")).toBe("560");
  });
  it("resource-list separator commits independently and supports keyboard resizing", () => {
    localStorage.setItem("tool-side-panel-width", "700");
    renderPanel();
    const nav = screen.getByRole("separator", { name: "调整资源列表宽度" });
    fireEvent(nav, pointer("pointerdown", 500));
    fireEvent(window, pointer("pointermove", 520));
    expect(nav).toHaveAttribute("aria-valuenow", "200");
    expect(localStorage.getItem("tool-side-panel-nav-width")).toBe("180");
    fireEvent(window, pointer("pointerup", 525));
    expect(localStorage.getItem("tool-side-panel-nav-width")).toBe("205");
    fireEvent.keyDown(nav, { key: "ArrowLeft" });
    expect(nav).toHaveAttribute("aria-valuenow", "185");
    expect(localStorage.getItem("tool-side-panel-width")).toBe("700");
  });
  it.each(["hidden", "session", "view"])("%s changes cancel an owned drag", reason => {
    const props = { open: true, mode: "artifacts" as ToolSidePanelMode, artifacts: [], sessionId: "resize-owner",
      onClose: vi.fn(), onSelectTool: vi.fn(), onSelectArtifact: vi.fn(), onOpenArtifacts: vi.fn() };
    const view = render(<ToolSidePanel {...props} />);
    fireEvent(screen.getByRole("separator", { name: "调整工作面板宽度" }), pointer("pointerdown", 1000));
    fireEvent(window, pointer("pointermove", 900));
    view.rerender(<ToolSidePanel {...props} open={reason !== "hidden"} sessionId={reason === "session" ? "new-owner" : props.sessionId}
      mode={reason === "view" ? "plan" : props.mode} />);
    fireEvent(window, pointer("pointermove", 800));
    expect(view.container.querySelector("aside")).toHaveStyle({ width: "420px" });
    expect(localStorage.getItem("tool-side-panel-width")).toBe("420");
  });
});

describe("ToolSidePanel navigation collapse", () => {
  // Load the real lazy document workspace before DOM queries start their
  // one-second deadline on cold native runners.
  beforeAll(async () => { await import("../DocumentWorkbench"); }, 30_000);

  beforeEach(() => {
    localStorage.clear();
    vi.mocked(invoke).mockReset().mockResolvedValue([]);
    HTMLElement.prototype.scrollIntoView = vi.fn();
  });

  it("coordinates with the global sidebar without losing the user's preferred panel width", async () => {
    const originalViewport = window.innerWidth;
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
    localStorage.setItem("app-sidebar-width", "400");
    localStorage.setItem("tool-side-panel-width", "380");
    function CoordinatedPanel() {
      const layout = useAppSidebarLayout();
      return <SidebarLayoutContext.Provider value={layout.context}>
        <div ref={layout.bodyRef}>
          <button onClick={() => layout.setCollapsed(true)}>隐藏全局导航</button>
          <button onClick={() => layout.setCollapsed(false)}>恢复全局导航</button>
          <ToolSidePanel open mode="artifacts" artifacts={[]} onClose={vi.fn()}
            onSelectTool={vi.fn()} onSelectArtifact={vi.fn()} onOpenArtifacts={vi.fn()} />
        </div>
      </SidebarLayoutContext.Provider>;
    }
    try {
      const { container } = render(<CoordinatedPanel />);
      const panel = container.querySelector<HTMLElement>(".tool-side-panel")!;
      expect(panel).toHaveClass("tool-side-panel--maximized");
      expect(localStorage.getItem("tool-side-panel-width")).toBe("380");
      fireEvent.click(screen.getByText("隐藏全局导航"));
      expect(panel).toHaveClass("tool-side-panel--maximized");
      fireEvent.click(screen.getByText("恢复全局导航"));
      expect(panel).toHaveClass("tool-side-panel--maximized");
      expect(localStorage.getItem("tool-side-panel-width")).toBe("380");
    } finally {
      Object.defineProperty(window, "innerWidth", { value: originalViewport });
    }
  });

  it("uses the whole default panel for a list or preview, with a return path", async () => {
    Object.defineProperty(window, "innerWidth", {configurable:true, value:1280});
    const { container } = renderPanel("artifacts", [artifact("C:/work/single.md")]);
    expect(container.querySelector(".task-details__list--split")).toBeNull();
    expect(screen.queryByRole("button", {name: "详情布局"})).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^single.md/ }));
    expect(screen.getByRole("button", {name: "返回成果列表"})).toBeInTheDocument();
    expect(container.querySelector(".task-details__list")).not.toBeVisible();
    fireEvent.click(screen.getByRole("button", {name: "返回成果列表"}));
    expect(screen.getByRole("button", {name: /^single.md/})).toBeInTheDocument();
  });

  it("closes the final standalone file without resurrecting it, then explicitly reopens the same path", async () => {
    Object.defineProperty(window, "innerWidth", {configurable:true,value:1280});
    vi.mocked(invoke).mockImplementation(async command => command === "read_text_file" ? "Standalone content" : []);
    const props = {open:true,mode:"fileTree" as const,artifacts:[],cwd:"/workspace",sessionId:"single-content-close",
      previewPath:"/workspace/note.md",onClose:vi.fn(),onSelectTool:vi.fn(),onSelectArtifact:vi.fn(),onOpenArtifacts:vi.fn()};
    const view = render(<ToolSidePanel {...props} previewRequestId={1} />);
    await screen.findByText("Standalone content");
    expect(screen.queryByRole("tablist",{name:"工作区文件和预览标签"})).toBeNull();
    fireEvent.click(screen.getByRole("button",{name:"关闭标签 note.md"}));
    expect(screen.queryByText("Standalone content")).toBeNull();
    expect(screen.queryByRole("button",{name:"关闭标签 note.md"})).toBeNull();
    view.rerender(<ToolSidePanel {...props} previewRequestId={2} />);
    expect(await screen.findByText("Standalone content")).toBeInTheDocument();
    expect(screen.getByRole("button",{name:"关闭标签 note.md"})).toBeInTheDocument();
  });

  it("keeps a lone file tab selectable without labelling a blank web preview as that file", async () => {
    Object.defineProperty(window, "innerWidth", {configurable:true,value:1280});
    vi.mocked(invoke).mockImplementation(async command => command === "read_text_file" ? "File preview content" : []);
    renderPanel("artifacts",[artifact("/workspace/context.md")]);
    fireEvent.click(screen.getByRole("button",{name:/^context.md/}));
    await screen.findByText("File preview content");
    fireEvent.click(screen.getByRole("tab",{name:"网页"}));
    const input=screen.getByRole("textbox",{name:"预览网址"});
    fireEvent.change(input,{target:{value:"https://example.com/draft"}});
    const tab=screen.getByRole("tab",{name:/context.md/});
    expect(tab).toHaveAttribute("aria-selected","false");
    fireEvent.click(tab);
    expect(screen.getByRole("tab",{name:"成果"})).toHaveAttribute("aria-selected","true");
    expect(await screen.findByText("File preview content")).toBeInTheDocument();
  });

  it("preserves expanded directories and the file list scroll through previews and category changes", async () => {
    Object.defineProperty(window, "innerWidth", {configurable:true,value:1280});
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "read_text_file") return "Nested file content";
      if (command !== "list_dir") return [];
      const path = (args as {path:string}).path;
      return [{name:path === "/workspace" ? "src" : "nested.md",path:`${path}/${path === "/workspace" ? "src" : "nested.md"}`,
        kind:path === "/workspace" ? "directory" : "file",size:1,modifiedAt:0,isLarge:false,isBinary:false}];
    });
    const props = {open:true,mode:"fileTree" as const,artifacts:[],cwd:"/workspace",sessionId:"tree-return",
      onClose:vi.fn(),onSelectTool:vi.fn(),onSelectArtifact:vi.fn(),onOpenArtifacts:vi.fn()};
    const {container,unmount} = render(<ToolSidePanel {...props} />);
    const directory = await screen.findByRole("treeitem",{name:/src/});
    fireEvent.click(directory);
    const nested = await screen.findByRole("treeitem",{name:/nested.md/});
    const list = container.querySelector<HTMLDivElement>(".task-details__list")!;
    list.scrollTop = 120;
    fireEvent.scroll(list);
    fireEvent.click(nested);
    await screen.findByText("Nested file content");
    expect(list).not.toBeVisible();
    fireEvent.click(screen.getByRole("button",{name:"返回文件列表"}));
    expect(screen.getByRole("treeitem",{name:/src/})).toBe(directory);
    expect(directory).toHaveAttribute("aria-expanded","true");
    expect(screen.getByRole("treeitem",{name:/nested.md/})).toBeVisible();
    expect(list.scrollTop).toBe(120);
    fireEvent.click(screen.getByRole("tab",{name:"网页"}));
    fireEvent.click(screen.getByRole("tab",{name:"文件"}));
    expect(directory).toHaveAttribute("aria-expanded","true");
    expect(list.scrollTop).toBe(120);
    unmount();
    render(<ToolSidePanel {...props} />);
    expect(await screen.findByRole("treeitem",{name:/nested.md/})).toBeVisible();
    expect(screen.getByRole("treeitem",{name:/src/})).toHaveAttribute("aria-expanded","true");
  });

  it("selects the shared artifact tab when the file tree opens a generated file", async () => {
    Object.defineProperty(window, "innerWidth", {configurable:true,value:1280});
    const generated = artifact("/workspace/generated.md");
    vi.mocked(invoke).mockImplementation(async command => command === "read_text_file" ? "Generated file content" : command === "list_dir"
      ? [{name:"generated.md",path:generated.path,kind:"file",size:1,modifiedAt:0,isLarge:false,isBinary:false},
        {name:"other.md",path:"/workspace/other.md",kind:"file",size:1,modifiedAt:0,isLarge:false,isBinary:false}] : []);
    render(<ToolSidePanel open mode="fileTree" artifacts={[generated]} cwd="/workspace" sessionId="generated-tree-selection"
      onClose={vi.fn()} onSelectTool={vi.fn()} onSelectArtifact={vi.fn()} onOpenArtifacts={vi.fn()} />);
    fireEvent.click(await screen.findByRole("treeitem",{name:/other.md/}));
    await screen.findByText("Generated file content");
    fireEvent.click(screen.getByRole("button",{name:"返回文件列表"}));
    fireEvent.click(screen.getByRole("treeitem",{name:/generated.md/}));
    const tabs = screen.getByRole("tablist",{name:"工作区文件和预览标签"});
    const active = within(tabs).getByRole("tab",{name:/generated.md/});
    expect(active).toHaveAttribute("aria-selected","true");
    expect(active).toHaveAttribute("tabindex","0");
    expect(within(tabs).getByRole("tab",{name:/other.md/})).toHaveAttribute("aria-selected","false");
    fireEvent.keyDown(active,{key:"ArrowLeft"});
    expect(within(tabs).getByRole("tab",{name:/other.md/})).toHaveAttribute("aria-selected","true");
  });

  it("exposes list layout controls only when a readable split fits", async () => {
    Object.defineProperty(window, "innerWidth", {configurable:true, value:1024});
    const { container } = renderPanel("fileTree");
    await screen.findByText("当前目录暂无可见文件。");
    expect(container.querySelector(".task-details__list--split")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", {name: "详情布局"}));
    fireEvent.click(screen.getByRole("menuitem", {name: "收起列表"}));
    expect(container.querySelector(".task-details__list--split")).toBeNull();
    fireEvent.click(screen.getByRole("button", {name: "详情布局"}));
    fireEvent.click(screen.getByRole("menuitem", {name: "钉住列表"}));
    expect(container.querySelector(".task-details__list--split")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", {name: "详情布局"}));
    expect(screen.getByRole("menuitem", {name: "收起列表"})).toBeDisabled();
  });

  it.each(["fileTree", "browser"] as const)(
    "repairs invalid persisted widths before rendering the %s view",
    async (mode) => {
      localStorage.setItem("tool-side-panel-width", "1");
      localStorage.setItem("tool-side-panel-nav-width", "36");

      const { container } = renderPanel(mode);
      const panel = container.querySelector<HTMLElement>(".tool-side-panel");
      const nav = container.querySelector<HTMLElement>(".tool-side-panel__nav");

      expect(panel).toHaveClass("tool-side-panel--maximized");
      if (mode === "fileTree") {
        expect(container.querySelector(".task-details__list--split")).toBeInTheDocument();
      } else {
        expect(nav).toBeNull();
        expect(screen.getByRole("textbox", { name: "预览网址" })).toBeInTheDocument();
      }

      await waitFor(() => {
        expect(localStorage.getItem("tool-side-panel-width")).toBe("280");
        expect(localStorage.getItem("tool-side-panel-nav-width")).toBe("180");
      });
    },
  );

  it("keeps browser navigation usable without a blank list column and can switch back to files", async () => {
    const { container } = renderPanel("browser");
    fireEvent.change(screen.getByRole("textbox", { name: "预览网址" }), { target: { value: "https://example.com/review" } });
    fireEvent.click(screen.getByRole("button", { name: "预览" }));
    expect(container.querySelector("iframe")).toHaveAttribute("src", "https://example.com/review");
    fireEvent.click(screen.getByRole("tab", { name: "文件" }));
    expect(await screen.findByText("当前目录暂无可见文件。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "详情布局" })).toBeInTheDocument();
  });

  it("previews an authorized generated Chinese PDF larger than the generic binary limit", async () => {
    const path = "C:/work/qa_office_f3.pdf";
    const generatedPdfBytes = 13_834_126;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "path_stat") return { absolute: path, exists: true, kind: "file" };
      if (command === "read_file_base64") {
        const limit = (args as { maxBytes: number }).maxBytes;
        if (limit < generatedPdfBytes) throw new Error("PDF exceeds the requested preview budget");
        return "JVBERi0xLjcK";
      }
      return undefined;
    });
    const { container } = renderPanel("artifacts", [artifact(path)]);
    fireEvent.click(screen.getByRole("button", { name: /^qa_office_f3\.pdf/ }));
    await waitFor(() => expect(container.querySelector("iframe[title='qa_office_f3.pdf']"))
      .toHaveAttribute("src", "data:application/pdf;base64,JVBERi0xLjcK"));
    expect(screen.queryByText(/无法在面板内预览/)).not.toBeInTheDocument();
  });

  it("waits for the persisted export receipt before reading a historical file", async () => {
    let authorize!: () => void;
    vi.mocked(invoke).mockImplementation(async command => {
      if (command === "session_export_authorize") return new Promise<void>(resolve => {authorize = resolve;});
      if (command === "read_text_file") return "restored export content";
      return undefined;
    });
    const exported = {...artifact("C:\\outside\\report.md"), kind: "document_export", toolCallId: `document-export:${"a".repeat(64)}`};
    renderPanel("artifacts", [exported]);
    fireEvent.click(screen.getByRole("button", {name: /^report\.md/}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("session_export_authorize", {sessionId: "session-1", path: exported.path, sha256: "a".repeat(64)}));
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "read_text_file")).toBe(false);
    await act(async () => {authorize();});
    expect(await screen.findByText("restored export content")).toBeInTheDocument();
  });

  it("does not read an old export when its receipt ACK arrives after the session switches", async () => {
    let authorize!: () => void;
    vi.mocked(invoke).mockImplementation(async command => {
      if (command === "session_export_authorize") return new Promise<void>(resolve => {authorize = resolve;});
      return undefined;
    });
    const exported = {...artifact("C:\\outside\\report.md"), kind: "document_export", toolCallId: `document-export:${"a".repeat(64)}`};
    const props = {open: true, mode: "artifacts" as const, cwd: "C:\\work", onClose: vi.fn(), onSelectTool: vi.fn(), onSelectArtifact: vi.fn(), onOpenArtifacts: vi.fn()};
    const {rerender} = render(<ToolSidePanel {...props} artifacts={[exported]} sessionId="first" />);
    fireEvent.click(screen.getByRole("button", {name: /^report\.md/}));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("session_export_authorize", expect.anything()));
    rerender(<ToolSidePanel {...props} artifacts={[]} sessionId="second" />);
    await act(async () => {authorize();});
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "read_text_file")).toBe(false);
  });

  it("keeps a changed export visible with a useful error and does not open or read it", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_export_authorize") throw new Error("导出文件已移动、删除或内容已变化，请重新导出");
      if (command === "document_workspace_open") return { sourceMissing: false, document: { revision: 1 } };
      throw new Error(`Unexpected native file access: ${command}`);
    });
    renderPanel("artifacts", [{...artifact("C:\\outside\\report.md"), kind: "document_export", toolCallId: `document-export:${"a".repeat(64)}`}]);
    fireEvent.click(screen.getByRole("button", {name: /^report\.md/}));
    expect(await screen.findByRole("alert")).toHaveTextContent("请重新导出");
    expect(screen.queryByRole("button", {name: "用系统应用打开"})).not.toBeInTheDocument();
    expect(screen.queryByRole("button", {name: "导出文档"})).not.toBeInTheDocument();
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(["session_export_authorize", "document_workspace_open"]);
    expect(invoke).toHaveBeenCalledWith("document_workspace_open", expect.objectContaining({ recoverMissingOnly: true }));
  });

  it("explains external-file authorization instead of offering a broken open action", async () => {
    vi.mocked(invoke).mockRejectedValue("拒绝访问未授权的路径：C:\\outside\\report.md");
    renderPanel("artifacts", [artifact("C:\\outside\\report.md")]);

    fireEvent.click(screen.getByRole("button", { name: /^report\.md/ }));

    expect(await screen.findByText(/位于当前工作区之外/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "选择该文件并授权预览" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "用系统应用打开" })).not.toBeInTheDocument();
  });

  it("retries the preview after the user authorizes the exact external file", async () => {
    let textReads = 0;
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "read_text_file") {
        textReads += 1;
        if (textReads === 1) throw new Error("拒绝访问未授权的路径：C:\\outside\\report.md");
        return "preview ready";
      }
      if (command === "filesystem_pick_files") return ["C:\\outside\\report.md"];
      if (command === "path_stat") {
        return { path: "C:\\outside\\report.md", exists: true, kind: "file", absolute: "C:\\outside\\report.md" };
      }
      return undefined;
    });
    const onToast = vi.fn();
    renderPanel("artifacts", [artifact("C:\\outside\\report.md")], onToast);
    fireEvent.click(screen.getByRole("button", { name: /^report\.md/ }));
    fireEvent.click(await screen.findByRole("button", { name: "选择该文件并授权预览" }));

    expect(await screen.findByText("preview ready")).toBeInTheDocument();
    expect(textReads).toBe(2);
    expect(onToast).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "用系统应用打开" })).toBeInTheDocument();
  });

  it("关联工具入口只打开工具，不再次触发文件选择覆盖工具模式", async () => {
    vi.mocked(invoke).mockResolvedValue("preview ready");
    const tc: ToolCallView = { toolCallId: "write-1", kind: "write", title: "Write report.md", status: "completed", content: [] };
    const onSelectTool = vi.fn();
    const onSelectArtifact = vi.fn();
    render(<ToolSidePanel open mode="artifacts" artifacts={[artifact("C:\\work\\report.md")]} cwd="C:\\work" sessionId="associated-tool-review"
      onClose={vi.fn()} onSelectTool={onSelectTool} onSelectArtifact={onSelectArtifact} onOpenArtifacts={vi.fn()} findToolCall={() => tc} />);
    fireEvent.click(screen.getByRole("button", { name: /^report\.md/ }));
    await screen.findByText("preview ready");
    onSelectArtifact.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "查看关联工具" }));
    expect(onSelectTool).toHaveBeenCalledTimes(1);
    expect(onSelectTool).toHaveBeenCalledWith(tc);
    expect(onSelectArtifact).not.toHaveBeenCalled();
  });

  it("缺失关联工具记录时提示并保留文件预览", async () => {
    vi.mocked(invoke).mockResolvedValue("preview ready");
    const onSelectTool = vi.fn();
    const onToast = vi.fn();
    render(<ToolSidePanel open mode="artifacts" artifacts={[artifact("C:\\work\\report.md")]} cwd="C:\\work" sessionId="missing-tool-review"
      onClose={vi.fn()} onSelectTool={onSelectTool} onSelectArtifact={vi.fn()} onOpenArtifacts={vi.fn()} onToast={onToast} findToolCall={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: /^report\.md/ }));
    await screen.findByText("preview ready");
    fireEvent.click(screen.getByRole("button", { name: "查看关联工具" }));
    expect(onSelectTool).not.toHaveBeenCalled();
    expect(onToast).toHaveBeenCalledWith("关联工具记录已不可用，可继续预览文件内容");
    expect(screen.getByText("preview ready")).toBeInTheDocument();
  });
});

function artifact(path: string): SessionArtifact {
  return {
    id: path.replace(/\\/g, "/").toLowerCase(),
    path,
    kind: "edit",
    title: `Write ${path}`,
    toolCallId: "write-1",
    status: "completed",
    verifiedOutput: true,
  };
}

describe("tool details share the transcript's execution ownership", () => {
  beforeEach(() => { localStorage.clear(); vi.mocked(invoke).mockReset().mockResolvedValue([]); });
  const native: ToolCallView = { toolCallId: "native", kind: "other", toolName: "computer_screenshot", title: "读取应用界面", status: "in_progress", content: [] };
  const read: ToolCallView = { toolCallId: "read", kind: "read_file", title: "Read source.md", status: "in_progress", content: [] };
  const message: ChatMessage = { id: "reply", role: "assistant", promptId: "prompt", complete: true,
    parts: [native, read].map(toolCall => ({ kind: "tool_call", toolCall })) };
  const automation: AutomationStatus = { sessionId: "owner", mode: "computer_use", paused: false, allowPrivateNetwork: false,
    browser: { available: true }, computer: { available: true, platform: "macOS", screenCapture: true, inputControl: true },
    browserRunning: false, browserHasData: false, task: { phase: "running", reason: null, scope: "核对来源", authorized: true, generation: 1, promptId: "prompt" } };
  const callbacks = { onClose: vi.fn(), onSelectTool: vi.fn(), onSelectArtifact: vi.fn(), onOpenArtifacts: vi.fn() };

  it("keeps native details live between model turns and retires unrelated tools", () => {
    const view = (toolCall: ToolCallView, automationStatus = automation) => <ToolSidePanel {...callbacks}
      open mode="tool" artifacts={[]} sessionId="owner" messages={[message]} toolCall={toolCall} activeMessageId={null} automationStatus={automationStatus} />;
    const { container, rerender } = render(view(native));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("运行中");
    rerender(view(read));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
    rerender(view(native, { ...automation, task: { ...automation.task!, phase: "stopped", authorized: false } }));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
  });

  it("a native pause does not stop the model's file tool, and status changes update open details", () => {
    const active = { ...message, complete: false };
    const view = (toolCall: ToolCallView, phase: "running" | "paused") => <ToolSidePanel {...callbacks}
      open mode="tool" artifacts={[]} sessionId="owner" messages={[active]} toolCall={toolCall} activeMessageId="reply"
      automationStatus={{ ...automation, task: { ...automation.task!, phase, authorized: phase === "running" } }} />;
    const { container, rerender } = render(view(native, "running"));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("运行中");
    rerender(view(native, "paused"));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
    rerender(view(read, "paused"));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("运行中");
    rerender(view(native, "running"));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("运行中");
  });

  it("does not revive historical operations using a different session, prompt or later assistant", () => {
    const view = (automationStatus: AutomationStatus, messages = [message]) => <ToolSidePanel {...callbacks}
      open mode="tool" artifacts={[]} sessionId="owner" messages={messages} toolCall={native} activeMessageId={null} automationStatus={automationStatus} />;
    const { container, rerender } = render(view({ ...automation, sessionId: "another" }));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
    rerender(view({ ...automation, task: { ...automation.task!, promptId: "another" } }));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
    rerender(view(automation, [message, { ...message, id: "later", parts: [] }]));
    expect(container.querySelector(".tool-detail__status")).toHaveTextContent("结果待确认");
  });
});
