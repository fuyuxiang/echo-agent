import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CodeBlockActions } from "../CodeBlockActions";

const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
const originalExecCommand = Object.getOwnPropertyDescriptor(document, "execCommand");
const setClipboard = (value: unknown) => Object.defineProperty(navigator, "clipboard", { configurable: true, value });

describe("CodeBlockActions clipboard states", () => {
  beforeEach(() => setClipboard(undefined));
  afterEach(() => {
    if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
    else Reflect.deleteProperty(navigator, "clipboard");
    if (originalExecCommand) Object.defineProperty(document, "execCommand", originalExecCommand);
    else Reflect.deleteProperty(document, "execCommand");
  });

  it("无 Clipboard API 时使用 DOM fallback 并确认成功", async () => {
    const execCommand = vi.fn(() => true);
    Object.defineProperty(document, "execCommand", { configurable: true, value: execCommand });
    const onAction = vi.fn();
    render(<CodeBlockActions code="hello" language="text" onAction={onAction} />);
    fireEvent.click(screen.getByRole("button", { name: "复制" }));
    expect(await screen.findByRole("button", { name: "已复制" })).toBeInTheDocument();
    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(onAction).toHaveBeenCalledWith("copy", "hello", "text", undefined);
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("clipboard 拒绝后 fallback 返回 false 显示失败并可重试", async () => {
    setClipboard({ writeText: vi.fn().mockRejectedValue(new Error("denied")) });
    const execCommand = vi.fn(() => false);
    Object.defineProperty(document, "execCommand", { configurable: true, value: execCommand });
    const onAction = vi.fn();
    render(<CodeBlockActions code="hello" language="text" onAction={onAction} />);
    fireEvent.click(screen.getByRole("button", { name: "复制" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("复制失败");
    expect(onAction).not.toHaveBeenCalled();
    execCommand.mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: "复制失败，点击重试" }));
    expect(await screen.findByRole("button", { name: "已复制" })).toBeInTheDocument();
  });

  it("复制中禁止重复点击，完成后显示成功", async () => {
    let complete!: () => void;
    const writeText = vi.fn(() => new Promise<void>((resolve) => { complete = resolve; }));
    setClipboard({ writeText });
    render(<CodeBlockActions code="hello" language="text" />);
    fireEvent.click(screen.getByRole("button", { name: "复制" }));
    const pending = screen.getByRole("button", { name: "复制中…" });
    expect(pending).toBeDisabled();
    fireEvent.click(pending);
    expect(writeText).toHaveBeenCalledTimes(1);
    complete();
    expect(await screen.findByRole("button", { name: "已复制" })).toBeInTheDocument();
  });
});
