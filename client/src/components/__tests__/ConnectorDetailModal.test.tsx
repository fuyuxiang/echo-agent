import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ConnectorDetailModal } from "../experts-panel/connectors/ConnectorDetailModal";
import type { ConnectorItem } from "@/lib/types";

vi.mock("@/lib/agent-client", () => ({ connectorsReadMcpConfig: vi.fn().mockResolvedValue("") }));

const connector: ConnectorItem = {
  id: "copy-review", name: "复制验证连接器", desc: "隔离测试", source: "review",
  kind: "mcp", cat: "other", examplesZh: ["查找项目文档", "总结文档中的待办事项"],
};
const clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, "clipboard");
const commandDescriptor = Object.getOwnPropertyDescriptor(document, "execCommand");

function setClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value });
}

function setCopyCommand(value: unknown) {
  Object.defineProperty(document, "execCommand", { configurable: true, value });
}

function props(onToast: (message: string) => void, item = connector) {
  return { connector: item, root: "", onClose: vi.fn(), onConfigure: vi.fn(), onToast };
}

describe("ConnectorDetailModal example prompt copy", () => {
  beforeEach(() => { setClipboard(undefined); setCopyCommand(undefined); });
  afterEach(() => {
    if (clipboardDescriptor) Object.defineProperty(navigator, "clipboard", clipboardDescriptor);
    else Reflect.deleteProperty(navigator, "clipboard");
    if (commandDescriptor) Object.defineProperty(document, "execCommand", commandDescriptor);
    else Reflect.deleteProperty(document, "execCommand");
  });

  it("uses the DOM fallback without Clipboard API and reports actual success", async () => {
    const copy = vi.fn().mockReturnValue(true);
    setCopyCommand(copy);
    const onToast = vi.fn();
    render(<ConnectorDetailModal {...props(onToast)} />);
    const button = screen.getByRole("button", { name: "复制示例提问：查找项目文档" });
    button.focus();
    fireEvent.click(button);
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("已复制到剪贴板"));
    expect(copy).toHaveBeenCalledWith("copy");
    expect(document.querySelector("textarea")).toBeNull();
    expect(button).toHaveFocus();
    expect(button).toBeEnabled();
  });

  it("reports denied clipboard and failed fallback, then permits retry", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    const copy = vi.fn().mockReturnValueOnce(false).mockReturnValueOnce(true);
    setClipboard({ writeText });
    setCopyCommand(copy);
    const onToast = vi.fn();
    render(<ConnectorDetailModal {...props(onToast)} />);
    const button = screen.getByRole("button", { name: "复制示例提问：查找项目文档" });
    fireEvent.click(button);
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("复制失败，请检查剪贴板权限"));
    expect(onToast).not.toHaveBeenCalledWith("已复制到剪贴板");
    expect(button).toBeEnabled();
    fireEvent.click(button);
    await waitFor(() => expect(onToast).toHaveBeenLastCalledWith("已复制到剪贴板"));
    expect(writeText).toHaveBeenCalledTimes(2);
  });

  it("shows busy feedback and prevents concurrent prompt copy", async () => {
    let resolve!: () => void;
    const writeText = vi.fn(() => new Promise<void>((done) => { resolve = done; }));
    setClipboard({ writeText });
    const onToast = vi.fn();
    render(<ConnectorDetailModal {...props(onToast)} />);
    const first = screen.getByRole("button", { name: "复制示例提问：查找项目文档" });
    const second = screen.getByRole("button", { name: "复制示例提问：总结文档中的待办事项" });
    fireEvent.click(first);
    fireEvent.click(first);
    fireEvent.click(second);
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith("查找项目文档");
    expect(first).toHaveAttribute("aria-busy", "true");
    expect(first).toHaveTextContent("复制中…");
    expect(second).toBeDisabled();
    expect(onToast).not.toHaveBeenCalled();
    await act(async () => resolve());
    expect(onToast).toHaveBeenCalledWith("已复制到剪贴板");
    expect(first).toBeEnabled();
    expect(second).toBeEnabled();
  });

  it("ignores a stale copy after switching connector without clearing a new request", async () => {
    const finishes: Array<() => void> = [];
    setClipboard({ writeText: vi.fn(() => new Promise<void>((resolve) => { finishes.push(resolve); })) });
    const onToast = vi.fn();
    const { rerender, unmount } = render(<ConnectorDetailModal {...props(onToast)} />);
    fireEvent.click(screen.getByRole("button", { name: "复制示例提问：查找项目文档" }));
    rerender(<ConnectorDetailModal {...props(onToast, { ...connector, id: "new-connector" })} />);
    const button = screen.getByRole("button", { name: "复制示例提问：查找项目文档" });
    expect(button).toBeEnabled();
    fireEvent.click(button);
    await act(async () => finishes[0]());
    expect(onToast).not.toHaveBeenCalled();
    expect(button).toBeDisabled();
    unmount();
    await act(async () => finishes[1]());
    expect(onToast).not.toHaveBeenCalled();
  });
});
