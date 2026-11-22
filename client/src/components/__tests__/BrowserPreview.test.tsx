import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import type { BrowserPanelView } from "@/lib/automation-client";
import { BrowserPreview } from "../BrowserPreview";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const empty: BrowserPanelView = { tabs: [], activeId: null, external: false, allowPrivateNetwork: false, developmentEndpoints: [], page: null };
const start = "http://127.0.0.1:5173/start";
const next = "http://127.0.0.1:5173/next#details";

function loaded(url: string, canGoBack = false): BrowserPanelView {
  return {
    ...empty,
    tabs: [{ id: "target", url, title: "Example" }], activeId: "target", external: false,
    page: {
      document: { targetId: "target", loaderId: "document", url },
      canGoBack, canGoForward: false, error: false, frameId: url, image: "AA==", width: 420, height: 320,
    },
  };
}

describe("BrowserPreview", () => {
  let view: BrowserPanelView;
  beforeEach(() => {
    view = empty;
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    vi.mocked(invoke).mockReset().mockImplementation(async command => command === "automation_browser_view" ? view : undefined);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("only grants a local development port after explicit confirmation", async () => {
    render(<BrowserPreview sessionId="session" active />);
    fireEvent.change(screen.getByRole("textbox", { name: "预览网址" }), { target: { value: "127.0.0.1:5173/start" } });
    fireEvent.click(screen.getByRole("button", { name: "打开" }));
    expect(screen.getByRole("group", { name: "本地端口授权" })).toHaveTextContent("127.0.0.1:5173");
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "automation_browser_action")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "允许并打开" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("automation_browser_action", {
      sessionId: "session", targetId: null, action: { kind: "navigate", url: start, allowDevelopment: true },
    }));
  });

  it("tracks in-page navigation and history from a native tab without creating an iframe", async () => {
    view = loaded(start);
    const { container } = render(<BrowserPreview sessionId="session" active />);
    const input = screen.getByRole("textbox", { name: "预览网址" });
    await waitFor(() => expect(input).toHaveValue(start));
    expect(container.querySelector("iframe")).toBeNull();
    view = loaded(next, true);
    await waitFor(() => expect(input).toHaveValue(next));
    fireEvent.click(screen.getByRole("button", { name: "后退" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("automation_browser_action", {
      sessionId: "session", targetId: "target", action: { kind: "back" },
    }));
    fireEvent.click(screen.getByRole("button", { name: "用系统浏览器打开" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("open_url", { url: next }));
  });

  it("opens a chat link only once and routes it to the owning session", async () => {
    const request = { url: "https://example.com/review", sequence: 1 };
    const { rerender } = render(<BrowserPreview sessionId="session" active request={request} />);
    const dispatched = () => vi.mocked(invoke).mock.calls.filter(([command]) => command === "automation_browser_action");
    await waitFor(() => expect(dispatched()).toEqual([["automation_browser_action", {
      sessionId: "session", targetId: null, action: { kind: "open", url: request.url },
    }]]));
    rerender(<BrowserPreview sessionId="session" active={false} request={request} />);
    rerender(<BrowserPreview sessionId="session" active request={request} />);
    expect(dispatched()).toHaveLength(1);
  });

  it("provides pause-and-take-over while leaving system browser access available", async () => {
    view = loaded(start, true);
    const onTakeOver = vi.fn(async () => true);
    render(<BrowserPreview sessionId="session" active agentWorking onTakeOver={onTakeOver} />);
    await waitFor(() => expect(screen.getByRole("textbox", { name: "预览网址" })).toHaveValue(start));
    expect(screen.getByRole("button", { name: "打开" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "后退" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "用系统浏览器打开" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "暂停并接手" }));
    await waitFor(() => expect(onTakeOver).toHaveBeenCalledTimes(1));
  });

  it("keeps the requested URL available for external opening after native launch fails", async () => {
    const url = "https://example.com/target";
    vi.mocked(invoke).mockImplementation(async command => {
      if (command === "automation_browser_view") return empty;
      if (command === "automation_browser_action") throw new Error("浏览器未安装");
    });
    render(<BrowserPreview sessionId="session" active request={{ url, sequence: 1 }} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("浏览器未安装");
    expect(screen.getByRole("textbox", { name: "预览网址" })).toHaveValue(url);
    fireEvent.click(screen.getByRole("button", { name: "用系统浏览器打开" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("open_url", { url }));
  });

  it("exposes the granted port and allows revoking it during an Agent turn", async () => {
    view = { ...loaded(start), developmentEndpoints: ["127.0.0.1:5173"] };
    render(<BrowserPreview sessionId="session" active agentWorking />);
    await screen.findByText("已授权端口 127.0.0.1:5173");
    fireEvent.click(screen.getByRole("button", { name: "撤销本地端口授权" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("automation_browser_action", {
      sessionId: "session", targetId: null, action: { kind: "revokeDevelopment" },
    }));
  });

  it("only forwards a completed drag and lets Escape return keyboard focus to the toolbar", async () => {
    view = loaded("https://example.com/drag");
    render(<BrowserPreview sessionId="session" active />);
    const frame = await screen.findByRole("img", { name: "Example" });
    Object.defineProperties(frame, { complete: { value: true }, naturalWidth: { value: 420 }, naturalHeight: { value: 320 } });
    vi.spyOn(frame, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 420, 320));
    fireEvent.pointerUp(frame, { button: 0, pointerId: 1, clientX: 90, clientY: 90 });
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "automation_browser_action")).toBe(false);
    fireEvent.pointerDown(frame, { button: 0, pointerId: 1, clientX: 20, clientY: 25 });
    fireEvent.pointerMove(frame, { buttons: 1, pointerId: 1, clientX: 90, clientY: 90 });
    fireEvent.pointerUp(frame, { button: 0, pointerId: 1, clientX: 90, clientY: 90 });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("automation_browser_action", {
      sessionId: "session", targetId: "target", action: {
        kind: "input", document: view.page?.document, frameId: view.page?.frameId,
        input: { kind: "pointer", x: 20, y: 25, destination: [90, 90], button: "left", count: 1, modifiers: 0 },
      },
    }));
    fireEvent.click(screen.getByRole("button", { name: "使用网页键盘" }));
    const keyboard = screen.getByRole("textbox", { name: "网页键盘输入" });
    expect(keyboard).toHaveFocus();
    fireEvent.keyDown(keyboard, { key: "Escape", code: "Escape", keyCode: 27, shiftKey: true });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("automation_browser_action", {
      sessionId: "session", targetId: "target", action: {
        kind: "input", document: view.page?.document, frameId: view.page?.frameId,
        input: { kind: "key", key: "Escape", code: "Escape", virtualKey: 27, modifiers: 0 },
      },
    }));
    fireEvent.keyDown(keyboard, { key: "Escape" });
    expect(screen.getByRole("textbox", { name: "预览网址" })).toHaveFocus();
  });

});
