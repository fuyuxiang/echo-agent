import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ShareMenu } from "../ShareMenu";
import type { ChatMessage } from "@/stores/session-store";

// triggerDownload 在 jsdom 下会走真实 document,我们 mock 掉 share 模块的核心副作用。
vi.mock("@/lib/share", async () => {
  const actual = await vi.importActual<typeof import("@/lib/share")>("@/lib/share");
  return {
    ...actual,
    triggerDownload: vi.fn(),
  };
});
vi.mock("@/lib/agent-client", () => ({ exportTextFile: vi.fn(), openExternalUrl: vi.fn() }));

import { triggerDownload, buildMailtoUrl } from "@/lib/share";
import { exportTextFile } from "@/lib/agent-client";
import * as htmlExporter from "@/lib/export-html";

const messages: ChatMessage[] = [
  { id: "u1", role: "user", complete: true, parts: [{ kind: "text", text: "你好" }] },
  { id: "a1", role: "assistant", complete: true, parts: [{ kind: "text", text: "你好!" }] },
];

describe("ShareMenu", () => {
  beforeEach(() => {
    vi.mocked(triggerDownload).mockClear();
    vi.mocked(exportTextFile).mockReset();
  });

  it("点击「分享」展开菜单", () => {
    render(<ShareMenu messages={messages} />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    expect(screen.getByText("导出 Markdown")).toBeInTheDocument();
    expect(screen.getByText("通过邮件分享")).toBeInTheDocument();
  });

  it("导出 Markdown 触发 triggerDownload + onDone", async () => {
    const onDone = vi.fn();
    render(<ShareMenu messages={messages} title="T" onDone={onDone} />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    fireEvent.click(screen.getByText("导出 Markdown"));
    await waitFor(() => expect(triggerDownload).toHaveBeenCalledWith(
      expect.objectContaining({ filename: "T.md", mime: expect.stringContaining("text/markdown") }),
    ));
    expect(onDone).toHaveBeenCalledWith(expect.stringContaining("T.md"));
  });

  it("导出 HTML / 纯文本", async () => {
    render(<ShareMenu messages={messages} title="X" />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    fireEvent.click(screen.getByText("导出 HTML"));
    await waitFor(() => expect(triggerDownload).toHaveBeenCalledWith(expect.objectContaining({ filename: "X.html" })));
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    fireEvent.click(screen.getByText("导出纯文本"));
    await waitFor(() => expect(triggerDownload).toHaveBeenCalledWith(expect.objectContaining({ filename: "X.txt" })));
  });

  it("通过邮件分享调用 openUrl(mailto:…)", () => {
    const openUrl = vi.fn();
    render(<ShareMenu messages={messages} title="分享主题" openUrl={openUrl} />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    fireEvent.click(screen.getByText("通过邮件分享"));
    expect(openUrl).toHaveBeenCalledTimes(1);
    const url = openUrl.mock.calls[0][0] as string;
    expect(url.startsWith("mailto:?subject=")).toBe(true);
    expect(url).toContain(encodeURIComponent("分享主题"));
    // buildMailtoUrl 是纯函数,可用它交叉校验格式。
    expect(buildMailtoUrl("分享主题", "")).toContain("mailto:");
  });

  it("点击外部关闭菜单", () => {
    render(
      <div>
        <ShareMenu messages={messages} />
        <button>外部</button>
      </div>,
    );
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    expect(screen.getByText("导出 Markdown")).toBeInTheDocument();
    fireEvent.mouseDown(screen.getByText("外部"));
    expect(screen.queryByText("导出 Markdown")).toBeNull();
  });

  it("Escape 关闭菜单并将焦点还给分享按钮", () => {
    render(<ShareMenu messages={messages} />);
    const trigger = screen.getByRole("button", { name: "分享" });
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("group", { name: "分享选项" })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("group", { name: "分享选项" })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it.each([
    ["导出 Markdown", "md"], ["导出 HTML", "html"], ["导出纯文本", "txt"],
  ])("桌面端 %s 使用保存对话框，取消后可重试", async (label, extension) => {
    const onDone = vi.fn();
    vi.mocked(exportTextFile).mockResolvedValueOnce(null).mockResolvedValueOnce(`/saved/T.${extension}`);
    render(<ShareMenu messages={messages} title="T" saveToFile onDone={onDone} />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    fireEvent.click(screen.getByRole("button", { name: label }));
    await waitFor(() => expect(exportTextFile).toHaveBeenCalledWith(`T.${extension}`, expect.any(String), extension));
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
    expect(onDone).not.toHaveBeenCalled();
    expect(screen.getByRole("group", { name: "分享选项" })).toBeInTheDocument();
    expect(triggerDownload).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: label }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith(`已导出到 /saved/T.${extension}`));
    expect(screen.queryByRole("group", { name: "分享选项" })).toBeNull();
  });

  it("生成中防止重复导出，关闭后丢弃迟到结果", async () => {
    let finish!: (html: string) => void;
    const pending = new Promise<string>(resolve => { finish = resolve; });
    const renderer = vi.spyOn(htmlExporter, "renderShareHtml").mockReturnValueOnce(pending);
    const onDone = vi.fn();
    render(<ShareMenu messages={messages} title="T" saveToFile onDone={onDone} />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    const button = screen.getByRole("button", { name: "导出 HTML" });
    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => expect(renderer).toHaveBeenCalledTimes(1));
    expect(button).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent("正在准备导出文件");
    fireEvent.keyDown(document, { key: "Escape" });
    // Reopening must not make an earlier export current again.
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    await act(async () => { finish("<!DOCTYPE html><p>late</p>"); await pending; });
    expect(exportTextFile).not.toHaveBeenCalled();
    expect(triggerDownload).not.toHaveBeenCalled();
    expect(onDone).not.toHaveBeenCalled();
    expect(screen.queryByRole("status")).toBeNull();
    renderer.mockRestore();
  });

  it("保存失败给出反馈并允许重新导出", async () => {
    vi.mocked(exportTextFile).mockRejectedValueOnce(new Error("磁盘空间不足"));
    const onDone = vi.fn();
    render(<ShareMenu messages={messages} saveToFile onDone={onDone} />);
    fireEvent.click(screen.getByRole("button", { name: "分享" }));
    fireEvent.click(screen.getByRole("button", { name: "导出 Markdown" }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith("导出失败：磁盘空间不足"));
    expect(screen.getByRole("button", { name: "导出 Markdown" })).toBeEnabled();
  });
});
