import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentProps, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/lib/kb-source-storage", () => ({ hydrateKnowledgeSources: vi.fn(async () => {}) }));
vi.mock("@/lib/artifact-catalog", () => ({ indexTaskArtifacts: vi.fn() }));
vi.mock("@/stores/projects-store", async (original) => ({
  ...await original<object>(), hydrateProjectsFromBackend: vi.fn(async () => {}),
}));
vi.mock("../ThemeProvider", () => ({ ThemeProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("../TitleBar", () => ({ TitleBar: () => null }));
vi.mock("../HomePage", () => ({ HomePage: () => <div>首页</div> }));
vi.mock("../PlaceholderPage", () => ({ PlaceholderPage: () => null }));
vi.mock("../TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("../SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("../TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("../FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("../Sidebar", () => ({ Sidebar: ({ onSelect }: { onSelect: (id: string) => void }) => (
  <>
    <button onClick={() => onSelect("parent")}>打开母任务</button>
    <button onClick={() => onSelect("other")}>打开其他任务</button>
    <button onClick={() => onSelect("old-group-session")}>打开历史群会话</button>
  </>
) }));
vi.mock("@/features/groups/GroupWorkspace", () => ({ GroupWorkspace: () => <div data-testid="active-group">群聊视图</div> }));
vi.mock("../ChatView", () => ({ ChatView: (props: ComponentProps<typeof import("../ChatView").ChatView>) => {
  const child = props.title === "母任务" ? "child" : "grandchild";
  return <div>
    {props.headerLeading}
    {props.sessionRestoreError && <button onClick={props.onRetrySessionRestore}>重试恢复会话</button>}
    <span data-testid="active-task">{props.title}</span>
    <span data-testid="restored-row">{props.subagentScrollRestore?.subagentKey ?? ""}</span>
    {props.title !== "孙子代理记录" && <button onClick={() => void props.onOpenSubagentSession?.(child, "/workspace", {
      parentSessionId: props.title === "母任务" ? "parent" : "child",
      parentCwd: "/workspace", subagentKey: child, scrollTop: 360, rowOffset: 72,
    })}>打开子代理</button>}
  </div>;
} }));
vi.mock("@/lib/agent-client", async (original) => ({
  ...await original<object>(),
  agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(), agentListWorkspaces: vi.fn(async () => []),
  agentLoadSession: vi.fn(async () => "model-a"),
  internalReload: vi.fn(async () => {}), subscribeAgentEvents: vi.fn(async () => () => {}),
}));

import App from "@/App";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore } from "@/stores/projects-store";
import { useGroupStore } from "@/features/groups/group-store";

const catalog = [
  { sessionId: "parent", cwd: "/workspace", title: "母任务", currentModelId: "model-a" },
  { sessionId: "child", cwd: "/workspace", title: "子代理记录", currentModelId: "model-a", hidden: true, sessionKind: "subagent" },
  { sessionId: "grandchild", cwd: "/workspace", title: "孙子代理记录", currentModelId: "model-a", hidden: true, sessionKind: "subagent" },
  { sessionId: "other", cwd: "/workspace", title: "其他任务", currentModelId: "model-a" },
  { sessionId: "old-group-session", cwd: "/workspace", title: "旧成员上下文", currentModelId: "model-a", hidden: true },
];
const auth = { ready: true, runtimeReady: true, synchronized: true, providers: ["model-a"], runtimeModels: ["model-a"], defaultModelId: "model-a" };

async function start() {
  render(<App />);
  await screen.findByText("首页");
  await waitFor(() => expect(client.agentListAllSessions).toHaveBeenCalled());
  await act(async () => {});
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useSessionsStore.setState(useSessionsStore.getInitialState());
  useSessionStore.setState(useSessionStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] });
  useGroupStore.setState({ groups: [], current: null, currentId: null });
  vi.mocked(client.agentInit).mockResolvedValue({ ok: true, cwd: "/workspace", auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({ providers: [], models: [{ modelId: "model-a", name: "模型 A", providerId: "p" }] } as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue(catalog);
  vi.mocked(listen).mockImplementation(async () => () => {});
  vi.mocked(invoke).mockImplementation(async (command) => command === "notification_take_pending_opens" ? [] : null);
});

describe("子代理任务层级导航", () => {
  it("隐藏历史群会话即使已在普通目录中，也会打开所属群聊", async () => {
    const group = { id:"historical-group",members:[],messages:[],works:[] };
    vi.mocked(invoke).mockImplementation(async(command) => {
      if(command === "notification_take_pending_opens") return [];
      if(command === "group_list") return [group];
      if(command === "group_owner") return "historical-group";
      if(command === "group_snapshot") return group;
      return null;
    });
    await start();
    fireEvent.click(screen.getByRole("button", {name:"打开历史群会话"}));
    expect(await screen.findByTestId("active-group")).toHaveTextContent("群聊视图");
    expect(useGroupStore.getState().currentId).toBe("historical-group");
    expect(client.agentLoadSession).not.toHaveBeenCalledWith("old-group-session",expect.anything());
  });
  it("归属查询失败时保留当前页面，允许重试且不按普通会话加载", async () => {
    vi.mocked(invoke).mockImplementation(async(command) => {
      if(command === "group_owner") throw new Error("归属数据库暂不可读");
      return command === "notification_take_pending_opens" ? [] : null;
    });
    await start();
    fireEvent.click(screen.getByRole("button", {name:"打开历史群会话"}));
    await screen.findByText(/暂时无法确认会话归属/);
    expect(client.agentLoadSession).not.toHaveBeenCalled();
    expect(screen.getByText("首页")).toBeInTheDocument();
  });
  it("快速切换时忽略晚到的群归属查询，不覆盖新任务", async () => {
    let resolveOwner!: (group: string) => void;
    vi.mocked(invoke).mockImplementation(async(command,args) => {
      if(command === "group_owner" && (args as {sessionId?:string})?.sessionId === "old-group-session")
        return new Promise(resolve => {resolveOwner=resolve;});
      if(command === "notification_take_pending_opens" || command === "group_list") return [];
      return null;
    });
    await start();
    fireEvent.click(screen.getByRole("button",{name:"打开历史群会话"}));
    fireEvent.click(screen.getByRole("button",{name:"打开其他任务"}));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("其他任务"));
    await act(async () => resolveOwner("historical-group"));
    expect(screen.queryByTestId("active-group")).toBeNull();
    expect(screen.getByTestId("active-task")).toHaveTextContent("其他任务");
    expect(client.agentLoadSession).not.toHaveBeenCalledWith("old-group-session",expect.anything());
  });
  it.each([false, true])("嵌套子代理可逐级返回并定位原条目，需要重试=%s", async retry => {
    await start();
    fireEvent.click(screen.getByRole("button", { name: "打开母任务" }));
    expect(await screen.findByTestId("active-task")).toHaveTextContent("母任务");
    fireEvent.click(screen.getByRole("button", { name: "打开子代理" }));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("子代理记录"));
    expect(screen.getByRole("button", { name: /返回上级任务：母任务/ })).toBeInTheDocument();
    if (retry) vi.mocked(client.agentLoadSession).mockRejectedValueOnce(new Error("历史回放中断"));
    fireEvent.click(screen.getByRole("button", { name: "打开子代理" }));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("孙子代理记录"));
    if (retry) {
      fireEvent.click(await screen.findByRole("button", { name: "重试恢复会话" }));
      await waitFor(() => expect(screen.queryByRole("button", { name: "重试恢复会话" })).toBeNull());
    }
    fireEvent.click(screen.getByRole("button", { name: /返回上级任务：子代理记录/ }));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("子代理记录"));
    expect(screen.getByTestId("restored-row")).toHaveTextContent("grandchild");
    fireEvent.click(screen.getByRole("button", { name: /返回上级任务：母任务/ }));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("母任务"));
    expect(screen.getByTestId("restored-row")).toHaveTextContent("child");
    expect(screen.queryByRole("button", { name: /返回上级任务/ })).toBeNull();
  });

  it("普通任务切换会清除子代理返回入口", async () => {
    await start();
    fireEvent.click(screen.getByRole("button", { name: "打开母任务" }));
    await screen.findByTestId("active-task");
    fireEvent.click(screen.getByRole("button", { name: "打开子代理" }));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("子代理记录"));
    fireEvent.click(screen.getByRole("button", { name: "打开其他任务" }));
    await waitFor(() => expect(screen.getByTestId("active-task")).toHaveTextContent("其他任务"));
    expect(screen.queryByRole("button", { name: /返回上级任务/ })).toBeNull();
  });

  it("子代理记录无法打开时保留母任务，不产生失效的返回路径", async () => {
    vi.mocked(client.agentListAllSessions).mockResolvedValue(catalog.filter((item) => item.sessionId !== "child"));
    await start();
    fireEvent.click(screen.getByRole("button", { name: "打开母任务" }));
    await screen.findByTestId("active-task");
    const catalogReads = vi.mocked(client.agentListAllSessions).mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: "打开子代理" }));
    await waitFor(() => expect(client.agentListAllSessions).toHaveBeenCalledTimes(catalogReads + 1));
    expect(screen.getByTestId("active-task")).toHaveTextContent("母任务");
    expect(screen.queryByRole("button", { name: /返回上级任务/ })).toBeNull();
  });
});
