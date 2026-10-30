import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
const codingGuard = vi.hoisted(() => vi.fn<() => Promise<boolean>>());
vi.mock("@/lib/kb-source-storage", () => ({ hydrateKnowledgeSources: vi.fn(async () => {}) }));
vi.mock("@/lib/artifact-catalog", () => ({ indexTaskArtifacts: vi.fn() }));
vi.mock("@/stores/projects-store", async original => ({
  ...await original<object>(), hydrateProjectsFromBackend: vi.fn(async () => {}),
}));
vi.mock("../ThemeProvider", () => ({ ThemeProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("../TitleBar", () => ({ TitleBar: () => null }));
vi.mock("../HomePage", () => ({ HomePage: () => <div>首页</div> }));
vi.mock("../TasksPanel", () => ({ TasksPanel: () => null }));
vi.mock("../SecondarySidebar", () => ({ SecondarySidebar: () => null }));
vi.mock("../TopbarActions", () => ({ TopbarActions: () => null }));
vi.mock("../FolderTrustDialog", () => ({ FolderTrustDialog: () => null }));
vi.mock("../Sidebar", () => ({ Sidebar: ({ onSelect, onNavigate }: {
  onSelect: (id: string) => void; onNavigate: (label: string) => void;
}) => <>
  <button onClick={() => onSelect("source")}>打开源会话</button>
  {['专家团', '项目', '代码开发'].map(label =>
    <button key={label} onClick={() => onNavigate(label)}>导航{label}</button>)}
</> }));
vi.mock("../ChatView", () => ({ ChatView: () => <div>真实会话已加载</div> }));
vi.mock("@/features/groups/GroupWorkspace", () => ({ GroupWorkspace: () => <div>专家团工作区</div> }));
vi.mock("../PlaceholderPage", () => ({ PlaceholderPage: ({ label, onRegisterCodingLeaveGuard }: {
  label: string;
  onRegisterCodingLeaveGuard?: (guard: (() => Promise<boolean>) | null) => void;
}) => {
  useEffect(() => {
    if (label !== "代码开发") return;
    onRegisterCodingLeaveGuard?.(codingGuard);
    return () => onRegisterCodingLeaveGuard?.(null);
  }, [label, onRegisterCodingLeaveGuard]);
  return <span>{label}页面</span>;
} }));
vi.mock("@/lib/agent-client", async original => ({
  ...await original<object>(), agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(), agentListWorkspaces: vi.fn(async () => []),
  agentLoadSession: vi.fn(async () => 'model-a'), subscribeAgentEvents: vi.fn(async () => () => {}),
  internalReload: vi.fn(async () => {}),
}));

const trayReview = vi.hoisted(() => ({ onAction: null as null | ((action: { id: string; kind: string; targetId?: string }) => Promise<void>) }));
vi.mock("@/lib/use-desktop-tray", () => ({ useDesktopTray: (_ready: boolean, action: typeof trayReview.onAction) => { trayReview.onAction = action; } }));
import { useGroupStore } from "@/features/groups/group-store";
import type { Group } from "@/features/groups/group-client";
import { TrayActionCancelled } from "@/lib/desktop-tray-connection";
import App from "@/App";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore } from "@/stores/projects-store";

const auth = { ready: true, runtimeReady: true, synchronized: true, providers: ['model-a'], runtimeModels: ['model-a'], defaultModelId: 'model-a' };
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  codingGuard.mockReset().mockResolvedValue(true);
  useGroupStore.setState({ groups: [], currentId: null, current: null, error: null });
  useSessionsStore.setState(useSessionsStore.getInitialState()); useSessionStore.setState(useSessionStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] });
  vi.mocked(client.agentInit).mockResolvedValue({ ok: true, cwd: '/workspace', auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({ providers: [], models: [{ modelId: 'model-a', name: '模型 A', providerId: 'p' }] } as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue([{ sessionId: 'source', title: '源会话', cwd: '/workspace', currentModelId: 'model-a' }] as never);
  vi.mocked(invoke).mockImplementation(async command => ['notification_take_pending_opens', 'coding_task_list'].includes(command) ? [] : null);
});
async function start() {
  render(<App />); await screen.findByText('首页'); await waitFor(() => expect(client.agentListAllSessions).toHaveBeenCalled());
  await act(async () => {}); fireEvent.click(screen.getByRole('button', { name: '打开源会话' }));
  await screen.findByText('真实会话已加载');
  act(() => useSessionStore.getState().setMessages([{ id: 'user', role: 'user', complete: true, parts: [{ kind: 'text', text: '保留当前上下文' }] }]));
}
const groupFixture = (id = 'test-team'): Group => ({
  id, name: id, goal: '产品评审', cwd: '/workspace', sourceSessionId: null,
  sourceTeamId: null, coordinatorId: 'coordinator', members: [], messages: [],
  works: [], decisions: [], archived: false, pinned: false, paused: false,
  updatedAt: 1, revision: 1, maxCalls: 10, tokenBudget: 100000,
});
function prepareGroup(group: Group, snapshot: () => Promise<Group>) {
  vi.mocked(invoke).mockImplementation(async command => command === 'group_snapshot' ? snapshot() : ['notification_take_pending_opens', 'coding_task_list'].includes(command) ? [] : null);
  act(() => useGroupStore.setState({ groups: [group], current: null, currentId: null, error: null }));
}
const openGroup = (group: Group) => trayReview.onAction!({ id: '1', kind: 'group', targetId: group.id });

describe('desktop tray group navigation', () => {
  it('opens the requested group after its native snapshot is loaded', async () => {
    await start();
    const group = groupFixture();
    prepareGroup(group, async () => group);
    await act(async () => { await openGroup(group); });
    await screen.findByText('专家团工作区');
    expect(useGroupStore.getState().current?.id).toBe(group.id);
  });

  it('preserves a newer page selection when the group snapshot finishes late', async () => {
    await start();
    const group = groupFixture();
    let finish!: (value: Group) => void;
    const gate = new Promise<Group>(resolve => { finish = resolve; });
    prepareGroup(group, () => gate);
    let opening!: Promise<void>;
    act(() => { opening = openGroup(group); });
    const cancelled = expect(opening).rejects.toBeInstanceOf(TrayActionCancelled);
    fireEvent.click(screen.getByRole('button', { name: '导航项目' }));
    await screen.findByText('项目页面');
    await act(async () => { finish(group); await cancelled; });
    expect(screen.getByText('项目页面')).toBeInTheDocument();
    expect(screen.queryByText('专家团工作区')).not.toBeInTheDocument();
  });

  it('preserves a newer session selection when the group snapshot finishes late', async () => {
    await start();
    const group = groupFixture();
    let finish!: (value: Group) => void;
    const gate = new Promise<Group>(resolve => { finish = resolve; });
    prepareGroup(group, () => gate);
    let opening!: Promise<void>;
    act(() => { opening = openGroup(group); });
    const cancelled = expect(opening).rejects.toBeInstanceOf(TrayActionCancelled);
    fireEvent.click(screen.getByRole('button', { name: '打开源会话' }));
    await act(async () => {});
    await act(async () => { finish(group); await cancelled; });
    expect(screen.getByText('真实会话已加载')).toBeInTheDocument();
    expect(useSessionsStore.getState().currentSessionId).toBe('source');
    expect(screen.queryByText('专家团工作区')).not.toBeInTheDocument();
  });

  it('keeps the current page on failure and allows a fresh successful retry', async () => {
    await start();
    const group = groupFixture();
    const snapshot = vi.fn().mockRejectedValueOnce(new Error('读取失败')).mockResolvedValue(group);
    prepareGroup(group, snapshot);
    await act(async () => { await expect(openGroup(group)).rejects.toThrow('读取失败'); });
    expect(screen.getByText('真实会话已加载')).toBeInTheDocument();
    await act(async () => { await openGroup(group); });
    await screen.findByText('专家团工作区');
  });

  it('rejects archived groups before starting a native load', async () => {
    await start();
    const group = { ...groupFixture(), archived: true };
    const snapshot = vi.fn().mockResolvedValue(group);
    prepareGroup(group, snapshot);
    await act(async () => { await expect(openGroup(group)).rejects.toThrow('专家团已归档'); });
    expect(snapshot).not.toHaveBeenCalled();
    expect(screen.getByText('真实会话已加载')).toBeInTheDocument();
  });

  it('preserves unsaved Coding content when leaving is cancelled', async () => {
    await start();
    const group = groupFixture();
    const snapshot = vi.fn().mockResolvedValue(group);
    prepareGroup(group, snapshot);
    fireEvent.click(screen.getByRole('button', { name: '导航代码开发' }));
    await screen.findByText('代码开发页面');
    codingGuard.mockResolvedValue(false);
    await act(async () => { await expect(openGroup(group)).rejects.toBeInstanceOf(TrayActionCancelled); });
    expect(snapshot).not.toHaveBeenCalled();
    expect(screen.getByText('代码开发页面')).toBeInTheDocument();
  });

  it('cancels a pending group navigation when a newer action wins during the leave guard', async () => {
    await start();
    const group = groupFixture();
    const snapshot = vi.fn().mockResolvedValue(group);
    prepareGroup(group, snapshot);
    fireEvent.click(screen.getByRole('button', { name: '导航代码开发' }));
    await screen.findByText('代码开发页面');
    let allow!: (value: boolean) => void;
    codingGuard.mockImplementationOnce(() => new Promise(resolve => { allow = resolve; }));
    let opening!: Promise<void>;
    act(() => { opening = openGroup(group); });
    const cancelled = expect(opening).rejects.toBeInstanceOf(TrayActionCancelled);
    await act(async () => { await trayReview.onAction!({ id: '2', kind: 'new' }); });
    await screen.findByText('首页');
    await act(async () => { allow(true); await cancelled; });
    expect(snapshot).not.toHaveBeenCalled();
    expect(screen.getByText('首页')).toBeInTheDocument();
  });

  it('preserves a newer page navigation while a new-chat leave guard is pending', async () => {
    await start();
    fireEvent.click(screen.getByRole('button', { name: '导航代码开发' }));
    await screen.findByText('代码开发页面');
    let allow!: (value: boolean) => void;
    codingGuard.mockImplementationOnce(() => new Promise(resolve => { allow = resolve; }));
    let opening!: Promise<void>;
    act(() => { opening = trayReview.onAction!({ id: '1', kind: 'new' }); });
    const cancelled = expect(opening).rejects.toBeInstanceOf(TrayActionCancelled);
    fireEvent.click(screen.getByRole('button', { name: '导航项目' }));
    await screen.findByText('项目页面');
    await act(async () => { allow(true); await cancelled; });
    expect(screen.getByText('项目页面')).toBeInTheDocument();
    expect(screen.queryByText('首页')).not.toBeInTheDocument();
  });
});
