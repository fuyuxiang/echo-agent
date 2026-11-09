import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSessionWorkspace } from "@/lib/use-session-workspace";
import { agentSessionWorkspaceStatus, filesystemPickDirectory } from "@/lib/agent-client";
import { SessionWorkspaceDirectory, SessionWorkspaceWarning } from "../SessionWorkspace";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock("@/lib/tauri-kb-reader", () => ({ isTauriAvailable: () => true }));
vi.mock("@/lib/agent-client", () => ({ agentSessionWorkspaceStatus: vi.fn(), filesystemPickDirectory: vi.fn() }));
const reload = vi.fn(async () => {});
const toast = vi.fn();
const newTask = vi.fn();
const openDirectory = vi.fn();
function Fixture({ id = "s1", cwd = "/original" }: { id?: string; cwd?: string }) {
  const workspace = useSessionWorkspace(id, cwd, null, reload, toast);
  return <><SessionWorkspaceWarning workspace={workspace} onNewTask={newTask} />
    <SessionWorkspaceDirectory workspace={workspace} onOpen={openDirectory} />
    <button disabled={!workspace.available || workspace.busy}>发送任务</button></>;
}
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(agentSessionWorkspaceStatus).mockReset().mockImplementation(async (_id, cwd) => ({ cwd, status: "ready" }));
  vi.mocked(filesystemPickDirectory).mockReset().mockResolvedValue(null);
});

describe("fixed session workspace", () => {
  it("opens the bound directory directly from Files", async () => {
    render(<Fixture />);
    await waitFor(() => expect(screen.getByRole("button", { name: "发送任务" })).toBeEnabled());
    expect(screen.getByText("/original")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "打开文件夹" }));
    expect(openDirectory).toHaveBeenCalledOnce();
  });

  it("preserves a missing directory, blocks send, and reloads the same session on recovery", async () => {
    vi.mocked(agentSessionWorkspaceStatus).mockResolvedValueOnce({ cwd: "/original", status: "missing", message: "not found" });
    render(<Fixture />);
    expect(await screen.findByRole("alert")).toHaveTextContent("工作目录已找不到");
    expect(screen.getByRole("button", { name: "发送任务" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "打开文件夹" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重试连接" }));
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole("button", { name: "发送任务" })).toBeEnabled());
    expect(screen.getByRole("button", { name: "打开文件夹" })).toBeEnabled();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(vi.mocked(agentSessionWorkspaceStatus).mock.calls.every(([id, cwd]) => id === "s1" && cwd === "/original")).toBe(true);
  });

  it("reauthorizes only the original directory; another folder starts a separate task", async () => {
    vi.mocked(agentSessionWorkspaceStatus).mockResolvedValue({ cwd: "/original", status: "unavailable" });
    vi.mocked(filesystemPickDirectory).mockResolvedValueOnce("/other").mockResolvedValueOnce(null);
    render(<Fixture />);
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "重新授权原目录" }));
    await waitFor(() => expect(toast).toHaveBeenCalledWith(expect.stringContaining("原目录")));
    expect(filesystemPickDirectory).toHaveBeenCalledWith("/original");
    expect(reload).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "发送任务" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重新授权原目录" }));
    await waitFor(() => expect(filesystemPickDirectory).toHaveBeenCalledTimes(2));
    expect(reload).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByRole("button", { name: "新建任务" })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: "新建任务" }));
    expect(newTask).toHaveBeenCalledOnce();
  });

  it("reauthorization reloads history only after the original folder becomes available", async () => {
    vi.mocked(agentSessionWorkspaceStatus).mockResolvedValueOnce({ cwd: "/original", status: "unavailable" });
    vi.mocked(filesystemPickDirectory).mockResolvedValue("/original");
    render(<Fixture />);
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "重新授权原目录" }));
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole("button", { name: "发送任务" })).toBeEnabled());
  });

  it("ignores a late unavailable result from the previous session", async () => {
    let resolve!: (value: { cwd: string; status: "missing" }) => void;
    vi.mocked(agentSessionWorkspaceStatus).mockImplementationOnce(() => new Promise(accept => { resolve = accept; }));
    const view = render(<Fixture />);
    view.rerender(<Fixture id="s2" cwd="/second" />);
    await waitFor(() => expect(screen.getByRole("button", { name: "发送任务" })).toBeEnabled());
    await act(async () => resolve({ cwd: "/original", status: "missing" }));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("/second")).toBeInTheDocument();
  });

  it("does not enable sending when the directory exists but history has not loaded", async () => {
    vi.mocked(agentSessionWorkspaceStatus).mockResolvedValueOnce({cwd:"/original",status:"ready",loaded:false})
      .mockResolvedValueOnce({cwd:"/original",status:"ready",loaded:false})
      .mockResolvedValue({cwd:"/original",status:"ready",loaded:true});
    render(<Fixture />);
    expect(await screen.findByRole("alert")).toHaveTextContent("重新加载本会话");
    expect(screen.getByRole("button", {name:"发送任务"})).toBeDisabled();
    fireEvent.click(screen.getByRole("button", {name:"重试连接"}));
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole("button", {name:"发送任务"})).toBeEnabled());
  });

  it("旧任务的恢复请求不会锁住新任务，也不会把旧目录结果带入新任务", async () => {
    let finish!: () => void;
    reload.mockImplementationOnce(() => new Promise<void>(resolve => {finish=resolve;}));
    vi.mocked(agentSessionWorkspaceStatus).mockResolvedValueOnce({cwd:"/original",status:"missing"});
    const view = render(<Fixture />);
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", {name:"重试连接"}));
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", {name:"发送任务"})).toBeDisabled();
    view.rerender(<Fixture id="s2" cwd="/second" />);
    await waitFor(() => expect(screen.getByRole("button", {name:"发送任务"})).toBeEnabled());
    await act(async () => finish());
    expect(screen.getByText("/second")).toBeInTheDocument();
    expect(screen.queryByText("/original")).toBeNull();
    expect(screen.getByRole("button", {name:"发送任务"})).toBeEnabled();
  });
});
