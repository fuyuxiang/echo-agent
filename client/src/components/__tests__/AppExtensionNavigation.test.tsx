import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
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
  {['插件', '专家·技能·连接器', '专家团', '项目'].map(label =>
    <button key={label} onClick={() => onNavigate(label)}>导航{label}</button>)}
</> }));
vi.mock("../ChatView", () => ({ ChatView: () => <div>真实会话已加载</div> }));
vi.mock("@/features/groups/GroupWorkspace", () => ({ GroupWorkspace: () => <div>专家团工作区</div> }));
vi.mock("../PlaceholderPage", async () => {
  const { PluginsPanel } = await import("../PluginsPanel");
  return { PlaceholderPage: ({ label, sessionId, onNavigate }: {
    label: string; sessionId?: string; onNavigate?: (label: string) => void;
  }) => label === '插件' ? <PluginsPanel sessionId={sessionId} />
      : <><span>{label}页面</span><button onClick={() => onNavigate?.('插件')}>管理本地插件</button></> };
});
vi.mock("@/lib/agent-client", async original => ({
  ...await original<object>(), agentInit: vi.fn(), agentAuthStatus: vi.fn(), providersList: vi.fn(),
  agentListAllSessions: vi.fn(), agentListWorkspaces: vi.fn(async () => []),
  agentLoadSession: vi.fn(async () => 'model-a'), subscribeAgentEvents: vi.fn(async () => () => {}),
  internalReload: vi.fn(async () => {}), pluginsList: vi.fn(async () => ({ plugins: [{ id: "user/demo", name: "本地插件", enabled: true, trusted: true }], warnings: [] })),
  pluginsAction: vi.fn(async () => ({ status: "success" })),
}));

import App from "@/App";
import * as client from "@/lib/agent-client";
import { useSessionsStore } from "@/stores/sessions-store";
import { useSessionStore } from "@/stores/session-store";
import { useProjectsStore } from "@/stores/projects-store";

const auth = { ready: true, runtimeReady: true, synchronized: true, providers: ['model-a'], runtimeModels: ['model-a'], defaultModelId: 'model-a' };
beforeEach(() => {
  vi.clearAllMocks(); localStorage.clear();
  useSessionsStore.setState(useSessionsStore.getInitialState()); useSessionStore.setState(useSessionStore.getInitialState());
  useProjectsStore.setState({ ...useProjectsStore.getInitialState(), projects: [] });
  vi.mocked(client.agentInit).mockResolvedValue({ ok: true, cwd: '/workspace', auth } as never);
  vi.mocked(client.agentAuthStatus).mockResolvedValue(auth);
  vi.mocked(client.providersList).mockResolvedValue({ providers: [], models: [{ modelId: 'model-a', name: '模型 A', providerId: 'p' }] } as never);
  vi.mocked(client.agentListAllSessions).mockResolvedValue([{ sessionId: 'source', title: '源会话', cwd: '/workspace', currentModelId: 'model-a' }] as never);
  vi.mocked(invoke).mockImplementation(async command => command === 'notification_take_pending_opens' ? [] : null);
});
async function start() {
  render(<App />); await screen.findByText('首页'); await waitFor(() => expect(client.agentListAllSessions).toHaveBeenCalled());
  await act(async () => {}); fireEvent.click(screen.getByRole('button', { name: '打开源会话' }));
  await screen.findByText('真实会话已加载');
  act(() => useSessionStore.getState().setMessages([{ id: 'user', role: 'user', complete: true, parts: [{ kind: 'text', text: '保留当前上下文' }] }]));
}
describe('extension navigation preserves the live management context', () => {
  it.each(['插件', '专家·技能·连接器', '专家团'])('allows local plugin toggle after %s', async route => {
    await start(); const before = useSessionStore.getState().messages;
    fireEvent.click(screen.getByRole('button', { name: `导航${route}` }));
    if (route === '专家·技能·连接器') fireEvent.click(await screen.findByRole('button', { name: '管理本地插件' }));
    if (route === '专家团') {
      await screen.findByText('专家团工作区');
      expect(screen.getByRole('button', { name: '专家团' })).toHaveAttribute('aria-current', 'page');
      fireEvent.click(screen.getByRole('button', { name: '插件' }));
    }
    fireEvent.click(await screen.findByRole('button', { name: '禁用插件 本地插件' }));
    await waitFor(() => expect(client.pluginsAction).toHaveBeenCalledWith('source', { type: 'disable', pluginId: 'user/demo' }));
    expect(useSessionsStore.getState().currentSessionId).toBe('source');
    expect(useSessionStore.getState().messages).toEqual(before);
  });
  it('keeps the existing project navigation reset behavior', async () => {
    await start(); fireEvent.click(screen.getByRole('button', { name: '导航项目' }));
    await screen.findByText('项目页面'); expect(useSessionsStore.getState().currentSessionId).toBeNull();
    expect(useSessionStore.getState().messages).toEqual([]);
  });
});
