import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { isTauri, invoke } from "@tauri-apps/api/core";
import mermaid from "mermaid";
import { MarkdownPreMermaid } from "../MarkdownPreMermaid";
import { normalizeStandaloneMermaidSvg } from "../mermaid-svg";

const graph = "flowchart LR\nA-->B";
const svg = '<svg viewBox="0 0 2400 400" xmlns="http://www.w3.org/2000/svg"></svg>';
const standaloneSvg = normalizeStandaloneMermaidSvg(svg);

vi.mock("mermaid", () => ({
  default: {
    mermaidAPI: { getConfig: vi.fn(() => ({ secure: ["securityLevel"] })) },
    initialize: vi.fn(),
    render: vi.fn(async () => ({ svg })),
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: vi.fn(() => false),
  invoke: vi.fn(),
}));

describe("Mermaid 图表预览", () => {
  const originalCreate = Object.getOwnPropertyDescriptor(URL, "createObjectURL");
  const originalRevoke = Object.getOwnPropertyDescriptor(URL, "revokeObjectURL");

  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(isTauri).mockReturnValue(false);
    Object.defineProperty(URL, "createObjectURL", { configurable: true, value: vi.fn(() => "blob:diagram") });
    Object.defineProperty(URL, "revokeObjectURL", { configurable: true, value: vi.fn() });
  });

  afterEach(() => {
    cleanup();
    if (originalCreate) Object.defineProperty(URL, "createObjectURL", originalCreate);
    else Reflect.deleteProperty(URL, "createObjectURL");
    if (originalRevoke) Object.defineProperty(URL, "revokeObjectURL", originalRevoke);
    else Reflect.deleteProperty(URL, "revokeObjectURL");
  });

  it("单击图表可在应用内预览，并以原始逻辑尺寸查看宽图", async () => {
    render(<MarkdownPreMermaid content={graph} />);
    const trigger = await screen.findByRole("button", { name: "放大预览图表" });
    fireEvent.click(trigger);

    const dialog = screen.getByRole("dialog", { name: "图表预览" });
    const image = within(dialog).getByRole("img");
    expect(image).toHaveAttribute("src", "blob:diagram");
    expect(dialog.querySelector("svg")).toBeNull();

    fireEvent.click(within(dialog).getByRole("button", { name: "原始大小" }));
    expect(image).toHaveStyle({ width: "2400px", height: "400px" });
    fireEvent.click(within(dialog).getByRole("button", { name: "放大图片" }));
    expect(image).toHaveStyle({ width: "3000px", height: "500px" });
    fireEvent.click(within(dialog).getByRole("button", { name: "适应窗口" }));
    expect(within(dialog).getByRole("button", { name: "适应窗口" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "图表预览" })).toBeNull();
    expect(document.activeElement).toBe(trigger);

    fireEvent.click(screen.getByRole("button", { name: "放大" }));
    expect(screen.getByRole("dialog", { name: "图表预览" })).toBeInTheDocument();
  });

  it("保留宿主提供的预览回调", async () => {
    const onPreviewMermaid = vi.fn();
    render(<MarkdownPreMermaid content={graph} onPreviewMermaid={onPreviewMermaid} />);
    await screen.findByRole("button", { name: "放大预览图表" });
    fireEvent.click(screen.getByRole("button", { name: "放大" }));
    expect(onPreviewMermaid).toHaveBeenCalledWith(standaloneSvg, graph);
    expect(screen.queryByRole("dialog", { name: "图表预览" })).toBeNull();
  });

  it("为独立 SVG 禁用 HTML 标签并固定中文字体，避免节点文字按不同字体计算宽度", async () => {
    render(<MarkdownPreMermaid content={graph} />);
    await screen.findByRole("button", { name: "放大预览图表" });
    expect(mermaid.initialize).toHaveBeenCalledWith(expect.objectContaining({
      securityLevel: "strict",
      htmlLabels: false,
      fontFamily: expect.stringContaining("PingFang SC"),
      secure: expect.arrayContaining(["securityLevel", "htmlLabels", "fontFamily"]),
    }));
  });

  it("桌面端下载打开原生保存对话框，并显示实际保存结果", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(invoke).mockResolvedValue("/tmp/diagram.svg");
    render(<MarkdownPreMermaid content={graph} />);
    fireEvent.click(await screen.findByRole("button", { name: "下载" }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("export_text_file", {
      suggestedName: "diagram.svg",
      extension: "svg",
      content: standaloneSvg,
    }));
    expect(await screen.findByRole("status")).toHaveTextContent("已保存：/tmp/diagram.svg");
  });

  it("保存被取消或失败时明确反馈", async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(invoke).mockResolvedValueOnce(null).mockRejectedValueOnce(new Error("磁盘不可写"));
    render(<MarkdownPreMermaid content={graph} />);
    const download = await screen.findByRole("button", { name: "下载" });
    fireEvent.click(download);
    expect(await screen.findByRole("status")).toHaveTextContent("已取消保存");
    fireEvent.click(download);
    expect(await screen.findByRole("status")).toHaveTextContent("下载失败：磁盘不可写");
  });

  it("浏览器端下载在链接加入页面后触发，并延后释放 Blob 地址", async () => {
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      expect(document.body.contains(this)).toBe(true);
      expect(this.download).toBe("diagram.svg");
    });
    try {
      render(<MarkdownPreMermaid content={graph} />);
      fireEvent.click(await screen.findByRole("button", { name: "下载" }));
      expect(await screen.findByRole("status")).toHaveTextContent("已发起下载");
      expect(click).toHaveBeenCalledOnce();
      expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    } finally {
      click.mockRestore();
    }
  });
});
