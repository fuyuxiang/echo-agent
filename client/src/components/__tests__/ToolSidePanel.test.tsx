import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { ToolSidePanel, type ToolSidePanelMode } from "../ToolSidePanel";
import type { SessionArtifact } from "@/lib/session-artifacts";
import type { ToolCallView } from "@/stores/session-store";

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

describe("ToolSidePanel navigation collapse", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.mocked(invoke).mockReset().mockResolvedValue([]);
    HTMLElement.prototype.scrollIntoView = vi.fn();
  });

  it("replaces the full header with one accessible expand control", () => {
    const { container } = renderPanel();

    expect(screen.getByRole("button", { name: "收起导航" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "钉住左列" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "收起导航" }));

    expect(container.querySelector(".tool-side-panel__nav--collapsed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "展开导航" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "钉住左列" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "产物" })).not.toBeInTheDocument();
    expect(container.querySelector(".tool-side-panel__nav-body")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "展开导航" }));

    expect(container.querySelector(".tool-side-panel__nav--collapsed")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "收起导航" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "钉住左列" })).toBeInTheDocument();
  });

  it("keeps the navigation expanded when pinning and unpinning", async () => {
    const { container } = renderPanel("fileTree");
    await screen.findByText(/没有可见文件/);

    fireEvent.click(screen.getByRole("button", { name: "钉住左列" }));
    expect(screen.getByRole("button", { name: "取消钉住" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "收起导航" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "取消钉住" }));
    expect(screen.getByRole("button", { name: "钉住左列" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "收起导航" })).toBeInTheDocument();
    expect(container.querySelector(".tool-side-panel__nav--collapsed")).not.toBeInTheDocument();
    expect(container.querySelector(".tool-side-panel__nav-body")).toBeInTheDocument();
  });

  it.each(["fileTree", "browser"] as const)(
    "repairs invalid persisted widths before rendering the %s view",
    async (mode) => {
      localStorage.setItem("tool-side-panel-width", "1");
      localStorage.setItem("tool-side-panel-nav-width", "36");

      const { container } = renderPanel(mode);
      const panel = container.querySelector<HTMLElement>(".tool-side-panel");
      const nav = container.querySelector<HTMLElement>(".tool-side-panel__nav");

      expect(panel?.style.width).toBe("280px");
      expect(nav?.style.width).toBe("140px");
      expect(container.querySelector(".tool-side-panel__nav-body")).toBeInTheDocument();

      await waitFor(() => {
        expect(localStorage.getItem("tool-side-panel-width")).toBe("280");
        expect(localStorage.getItem("tool-side-panel-nav-width")).toBe("140");
      });
    },
  );

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
    expect(onToast).toHaveBeenCalledWith("已授权该文件，可以在面板中预览");
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
