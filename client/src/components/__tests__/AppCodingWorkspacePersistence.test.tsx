import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/lib/kb-source-storage", () => ({ hydrateKnowledgeSources: vi.fn(async () => {}) }));
vi.mock("@/lib/artifact-catalog", async original => ({ ...await original<object>(), indexTaskArtifacts: vi.fn() }));
vi.mock("@/stores/projects-store", async (original) => ({
  ...await original<object>(), hydrateProjectsFromBackend: vi.fn(async () => {}),
}));
vi.mock("../ThemeProvider", () => ({ ThemeProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("../TitleBar", () => ({ TitleBar: () => null }));
vi.mock("../HomePage", () => ({ HomePage: () => <div>首页</div> }));
vi.mock("../PlaceholderPage", () => ({
  PlaceholderPage: ({ label, cwd, codingWorkspaces = [], onSelectWorkspace, onRegisterCodingLeaveGuard }: {
    label: string;
    cwd?: string;
    codingWorkspaces?: Array<{ cwd: string; sessionCount: number; lastTitle?: string }>;
    onSelectWorkspace?: (cwd: string) => void;
    onRegisterCodingLeaveGuard?: (guard: (() => Promise<boolean>) | null) => void;
  }) => {
    useEffect(() => {
      if (label !== "代码开发") return;
      onRegisterCodingLeaveGuard?.(async () => true);
      return () => onRegisterCodingLeaveGuard?.(null);
    }, [label, onRegisterCodingLeaveGuard]);
    return label === "代码开发"
      ? <div data-testid="coding-frame">IDE<span data-testid="coding-cwd">{cwd}</span>
        <output data-testid="coding-projects">{JSON.stringify(codingWorkspaces)}</output>
        <button onClick={() => onSelectWorkspace?.("/isolated-worktree")}>打开并行工作树</button>
        {codingWorkspaces.map(workspace => <button key={workspace.cwd} onClick={() => onSelectWorkspace?.(workspace.cwd)}>切回 {workspace.cwd}</button>)}
      </div>
      : <div>项目页面</div>;
  },
}));
vi.mock("../Sidebar", () => ({
  Sidebar: ({ onNavigate }: { onNavigate: (label: string) => void }) => (
    <>
      <button onClick={() => onNavigate("代码开发")}>打开代码开发</button>
      <button onClick={() => onNavigate("项目")}>打开项目</button>
    </>
  ),
}));
vi.mock("../TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("../SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("../TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("../FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("@/lib/agent-client", async (original) => ({
  ...await original<object>(),
  agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(async () => []), agentListWorkspaces: vi.fn(async () => []),
  agentListSessionHistory: vi.fn(async () => ({
    sessions: await (await import("@/lib/agent-client")).agentListAllSessions(true),
    nextCursor: null,
    nextArchiveCursor: null,
    incomplete: false,
  })),
  agentFindSessionSummary: vi.fn(async (sessionId: string) =>
    (await (await import("@/lib/agent-client")).agentListAllSessions(true)).find(entry => entry.sessionId === sessionId) ?? null),
  subscribeAgentEvents: vi.fn(async () => () => {}),
}));

import App from "@/App";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useSessionsStore.setState(useSessionsStore.getInitialState());
  useSessionStore.setState(useSessionStore.getInitialState());
  const auth = { ready: true, runtimeReady: true, synchronized: true, providers: ["model-a"], runtimeModels: ["model-a"], defaultModelId: "model-a" };
  vi.mocked(client.agentInit).mockResolvedValue({ ok: true, cwd: "/repo", auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({ providers: [], models: [{ modelId: "model-a", name: "模型 A", providerId: "p" }] } as never);
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "notification_take_pending_opens" || command === "coding_task_list" ? [] : null);
  vi.mocked(listen).mockImplementation(async () => () => {});
});

describe("代码开发页面生命周期", () => {
  it("keeps the implicit original repository reachable and persistent after opening a worktree", async () => {
    render(<App />);
    await screen.findByText("首页");
    fireEvent.click(screen.getByText("打开代码开发"));
    await waitFor(() => expect(screen.getByTestId("coding-cwd")).toHaveTextContent("/repo"));
    fireEvent.click(screen.getByText("打开并行工作树"));
    await waitFor(() => expect(screen.getByTestId("coding-cwd")).toHaveTextContent("/isolated-worktree"));
    const projects = JSON.parse(screen.getByTestId("coding-projects").textContent!);
    expect(projects.map((workspace: { cwd: string }) => workspace.cwd)).toEqual(["/isolated-worktree", "/repo"]);
    await waitFor(() => expect(JSON.parse(localStorage.getItem("echo-coding-workspaces")!)).toEqual(projects));
    fireEvent.click(screen.getByText("切回 /repo"));
    await waitFor(() => expect(screen.getByTestId("coding-cwd")).toHaveTextContent("/repo"));
    expect(new Set(JSON.parse(screen.getByTestId("coding-projects").textContent!).map((workspace: { cwd: string }) => workspace.cwd)).size).toBe(2);
  });
  it("returns to the existing IDE surface after navigating to another page", async () => {
    render(<App />);
    await screen.findByText("首页");
    fireEvent.click(screen.getByText("打开代码开发"));
    const frame = await screen.findByTestId("coding-frame");
    const surface = frame.closest(".app__coding-surface");
    expect(surface).not.toHaveAttribute("hidden");

    fireEvent.click(screen.getByText("打开项目"));
    await screen.findByText("项目页面");
    expect(surface).toHaveAttribute("hidden");
    expect(frame).toBeInTheDocument();

    fireEvent.click(screen.getByText("打开代码开发"));
    await waitFor(() => expect(surface).not.toHaveAttribute("hidden"));
    expect(screen.getByTestId("coding-frame")).toBe(frame);
  });

  it("retains existing original workspace metadata when adding an implicit project", async () => {
    const original = { cwd: "/repo", sessionCount: 7, lastTitle: "中文原项目" };
    vi.mocked(client.agentListWorkspaces).mockResolvedValue([original]);
    render(<App />);
    await waitFor(() => expect(useSessionsStore.getState().workspaces).toContainEqual(original));
    fireEvent.click(screen.getByText("打开代码开发"));
    await screen.findByTestId("coding-frame");
    fireEvent.click(screen.getByText("打开并行工作树"));
    await waitFor(() => expect(JSON.parse(screen.getByTestId("coding-projects").textContent!)).toContainEqual(original));
  });
});
