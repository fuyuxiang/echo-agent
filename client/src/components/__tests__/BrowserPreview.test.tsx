import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { BrowserPreview } from "../BrowserPreview";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("BrowserPreview", () => {
  beforeEach(() => vi.mocked(invoke).mockReset());
  it("空 URL 显示输入提示", () => {
    render(<BrowserPreview url="" />);
    expect(screen.getByText("输入一个 https 网址以预览。")).toBeInTheDocument();
  });

  it("不可预览 URL 显示拒绝提示", () => {
    render(<BrowserPreview url="localhost" />);
    expect(screen.getByText(/不可预览/)).toBeInTheDocument();
  });

  it("合法 URL 渲染 <iframe>(带 sandbox)", () => {
    render(<BrowserPreview url="https://example.com" />);
    const iframe = document.querySelector("iframe") as HTMLIFrameElement;
    expect(iframe).not.toBeNull();
    expect(iframe.getAttribute("src")).toContain("example.com");
    expect(iframe.getAttribute("sandbox")).toContain("allow-scripts");
  });

  it("Enter 触发 onUrlChange(规整后)", () => {
    const onUrlChange = vi.fn();
    render(<BrowserPreview url="" onUrlChange={onUrlChange} />);
    const input = screen.getByRole("textbox", { name: "预览网址" });
    fireEvent.change(input, { target: { value: "example.com" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onUrlChange).toHaveBeenCalledWith("https://example.com/");
  });

  it("预览按钮 disabled 当 URL 无效", () => {
    render(<BrowserPreview url="localhost" />);
    expect(screen.getByRole("button", { name: "预览" })).toBeDisabled();
  });

  it("iframe title 用 hostname", () => {
    render(<BrowserPreview url="https://docs.example.com/x" />);
    expect(document.querySelector("iframe")?.title).toBe("docs.example.com");
  });

  it("中文组合输入的 Enter 不触发导航", () => {
    const onUrlChange = vi.fn();
    render(<BrowserPreview url="" onUrlChange={onUrlChange} />);
    const input = screen.getByRole("textbox", { name: "预览网址" });
    fireEvent.change(input, { target: { value: "example.com" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(onUrlChange).not.toHaveBeenCalled();
  });

  it("清空 URL 后移除上一标签页面", () => {
    const { rerender } = render(<BrowserPreview url="https://example.com" />);
    expect(document.querySelector("iframe")).not.toBeNull();
    rerender(<BrowserPreview url="" />);
    expect(document.querySelector("iframe")).toBeNull();
    expect(screen.getByRole("button", { name: "后退" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "用系统浏览器打开" })).toBeDisabled();
  });

  it("系统外开等待完成，失败后可重试", async () => {
    let fail!: (reason: Error) => void;
    vi.mocked(invoke).mockImplementationOnce(() => new Promise((_resolve, reject) => { fail = reject; }));
    render(<BrowserPreview url="https://example.com" />);
    const button = screen.getByRole("button", { name: "用系统浏览器打开" });
    fireEvent.click(button);
    expect(button).toBeDisabled();
    expect(screen.getByText("正在打开系统浏览器…")).toBeInTheDocument();
    fail(new Error("unavailable"));
    expect(await screen.findByRole("alert")).toHaveTextContent("打开失败");
    expect(button).toBeEnabled();
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    fireEvent.click(button);
    expect(await screen.findByText("已在系统浏览器中打开")).toBeInTheDocument();
  });

  it("后退前进保留已访问网址并在载入后移除等待状态", () => {
    render(<BrowserPreview url="https://example.com" />);
    fireEvent.change(screen.getByRole("textbox", { name: "预览网址" }), { target: { value: "https://docs.example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "预览" }));
    expect(document.querySelector("iframe")?.src).toContain("docs.example.com");
    fireEvent.click(screen.getByRole("button", { name: "后退" }));
    expect(document.querySelector("iframe")?.src).toBe("https://example.com/");
    fireEvent.click(screen.getByRole("button", { name: "前进" }));
    expect(document.querySelector("iframe")?.src).toContain("docs.example.com");
    fireEvent.load(document.querySelector("iframe")!);
    expect(screen.queryByText("正在加载网页…")).not.toBeInTheDocument();
  });
});
