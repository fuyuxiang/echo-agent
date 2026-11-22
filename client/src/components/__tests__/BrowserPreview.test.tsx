import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { BrowserPreview } from "../BrowserPreview";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const empty = { tabs: [], activeId: null, canGoBack: false, canGoForward: false, developmentEndpoints: [] };
const local = "http://127.0.0.1:5173/start";
const remote = "https://example.com/review";
const call = (kind: string) => vi.mocked(invoke).mock.calls.filter(([command, params]) =>
  command === "browser_panel" && (params as { action?: { kind: string } })?.action?.kind === kind,
);
const click = (name: string) => fireEvent.click(screen.getByRole("button", { name }));
const address = () => screen.getByRole("textbox", { name: "预览网址" });

describe("BrowserPreview", () => {
  beforeEach(() => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    vi.mocked(invoke).mockReset().mockImplementation(async () => empty);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("requires confirmation before navigating to a local service", async () => {
    render(<BrowserPreview sessionId="session" active />);
    fireEvent.change(address(), { target: { value: local } });
    click("打开网址");
    expect(screen.getByRole("group", { name: "本地端口授权" })).toHaveTextContent("127.0.0.1:5173");
    expect(call("open")).toHaveLength(0);
    click("允许并打开");
    await waitFor(() => expect(call("open")).toEqual([["browser_panel", expect.objectContaining({
      sessionId: "session", action: { kind: "open", url: local, allowDevelopment: true },
    })]]));
  });

  it("opens an explicit link once and displays a native tab title without an iframe or screenshot", async () => {
    vi.mocked(invoke).mockImplementation(async () => ({
      ...empty, tabs: [{ id: 1, url: remote, title: "Preview" }], activeId: 1, canGoBack: true,
    }));
    const request = { kind: "open" as const, url: remote, sequence: 1 };
    const { container, rerender } = render(<BrowserPreview sessionId="session" active request={request} />);
    expect(await screen.findByRole("button", { name: "Preview" })).toHaveAttribute("aria-pressed", "true");
    expect(address()).toHaveValue(remote);
    expect(container.querySelector("iframe,img")).toBeNull();
    click("后退");
    await waitFor(() => expect(call("back")).toHaveLength(1));
    rerender(<BrowserPreview sessionId="session" active={false} request={request} />);
    rerender(<BrowserPreview sessionId="session" active request={request} />);
    await waitFor(() => expect(call("layout").some(([, args]) =>
      (args as { action: { bounds: object | null } }).action.bounds === null)).toBe(true));
    expect(call("open")).toHaveLength(1);
  });

  it("loads an HTML artifact by workspace path", async () => {
    render(<BrowserPreview sessionId="session" active request={{ kind: "artifact", path: "site/index.html", sequence: 1 }} />);
    await waitFor(() => expect(call("artifact")).toEqual([["browser_panel", expect.objectContaining({
      sessionId: "session", action: { kind: "artifact", path: "site/index.html" },
    })]]));
  });

  it("uses system browsing and the source viewer when the native panel is unsupported", async () => {
    const showSource = vi.fn();
    render(<BrowserPreview sessionId="session" active panelCapabilities={{ web: false, artifacts: false }}
      request={{ kind: "artifact", path: "index.html", sequence: 1 }} onShowArtifactSource={showSource} />);
    expect(showSource).toHaveBeenCalledWith("index.html");
    fireEvent.change(address(), { target: { value: remote } });
    click("打开网址");
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("open_url", { url: remote }));
    expect(call("open")).toHaveLength(0);
    expect(call("artifact")).toHaveLength(0);
  });

  it("keeps input locked until the native panel acknowledges takeover", async () => {
    const pause = vi.fn(async () => true);
    const page = { url: remote, shared: true };
    const locked = { ...empty, inputLocked: true, tabs: [{ id: 1, url: remote, title: "Preview" }], activeId: 1 };
    vi.mocked(invoke).mockImplementation(async (command, params) => command === "browser_panel"
      && (params as { action: { kind: string } }).action.kind === "view" ? { ...locked, inputLocked: false } : locked);
    const { rerender } = render(<BrowserPreview sessionId="session" active agentWorking taskPage={page} onTakeOver={pause} />);
    expect(await screen.findByRole("button", { name: "Preview" })).toBeDisabled();
    click("暂停并接手");
    await waitFor(() => expect(call("view")).toHaveLength(1));
    rerender(<BrowserPreview sessionId="session" active taskPage={page} onTakeOver={pause} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Preview" })).toBeEnabled());
    expect(call("open")).toHaveLength(0);
  });

  it("explains the account-wide impact before closing public tabs and clearing data", async () => {
    vi.mocked(invoke).mockResolvedValue({ ...empty, canClearBrowsingData: true, profileTabCount: 3 });
    render(<BrowserPreview sessionId="session" active />);
    click("网页工具");
    await waitFor(() => expect(screen.getByRole("button", { name: "清除浏览数据" })).toBeEnabled());
    click("清除浏览数据");
    expect(screen.getByRole("group", { name: "清除浏览数据确认" })).toHaveTextContent("各会话中的公网网页标签（3 个）");
    expect(call("clearBrowsingData")).toHaveLength(0);
    click("清除");
    await waitFor(() => expect(call("clearBrowsingData")).toHaveLength(1));
  });

  it("offers the actual task browser after pausing instead of opening a new copy of the URL", async () => {
    const pause = vi.fn(async () => true);
    render(<BrowserPreview sessionId="session" active agentWorking taskPage={{ title: "Task", url: remote }} onTakeOver={pause} />);
    click("暂停并打开任务浏览器");
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("automation_focus_browser", { sessionId: "session" }));
    expect(pause).toHaveBeenCalledOnce();
    expect(call("open")).toHaveLength(0);
  });

  it("leaves a shared task tab in place and re-enables navigation after takeover", async () => {
    const pause = vi.fn(async () => true);
    vi.mocked(invoke).mockImplementation(async () => ({
      ...empty, tabs: [{ id: 1, url: remote, title: "Preview" }], activeId: 1,
    }));
    const taskPage = { title: "Task", url: remote, shared: true };
    const { rerender } = render(<BrowserPreview sessionId="session" active agentWorking taskPage={taskPage} onTakeOver={pause} />);
    expect(await screen.findByRole("button", { name: "Preview" })).toBeDisabled();
    click("暂停并接手");
    await waitFor(() => expect(pause).toHaveBeenCalledOnce());
    expect(invoke).not.toHaveBeenCalledWith("automation_focus_browser", { sessionId: "session" });
    expect(call("open")).toHaveLength(0);
    rerender(<BrowserPreview sessionId="session" active taskPage={taskPage} onTakeOver={pause} />);
    expect(screen.getByRole("button", { name: "打开网址" })).toBeEnabled();
  });

  it("saves a selected HTML element and hands the same selection to the chat when requested", async () => {
    const artifactPath = "/workspace/index.html";
    const onEditArtifact = vi.fn();
    const view = { ...empty, tabs: [{ id: 1, url: "index.html", title: "产物", artifactPath }], activeId: 1 };
    const selection = {
      token: "selection-1", tag: "h1", id: "headline", label: "原始标题",
      text: "原始标题", image: null, color: "#171717", fontSize: 24, bold: true,
    };
    vi.mocked(invoke).mockImplementation(async (_command, params) =>
      (params as { action?: { kind: string } })?.action?.kind === "editSelection" ? { ...view, selection } : view);
    render(<BrowserPreview sessionId="session" active onEditArtifact={onEditArtifact} />);
    fireEvent.click(await screen.findByRole("button", { name: "编辑网页" }));
    const field = await screen.findByRole("textbox", { name: "元素文字" });
    expect(field).toHaveValue("原始标题");
    click("让 Agent 修改");
    expect(onEditArtifact).toHaveBeenCalledWith({ path: artifactPath, tag: "h1", id: "headline", label: "原始标题" });
    fireEvent.change(field, { target: { value: "更新标题" } });
    click("保存");
    await waitFor(() => expect(call("editSave")).toEqual([["browser_panel", expect.objectContaining({
      action: { kind: "editSave", token: "selection-1", change: { text: "更新标题" } },
    })]]));
    expect(await screen.findByRole("status")).toHaveTextContent("已保存到 HTML 产物");
  });

  it("keeps the requested URL available for the system browser after a native load error", async () => {
    vi.mocked(invoke).mockImplementation(async (_command, params) => {
      if ((params as { action?: { kind: string } })?.action?.kind === "open") throw new Error("网页未能打开");
      return empty;
    });
    render(<BrowserPreview sessionId="session" active request={{ kind: "open", url: remote, sequence: 1 }} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("网页未能打开");
    expect(address()).toHaveValue(remote);
    click("在系统浏览器中打开");
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("open_url", { url: remote }));
  });
});
