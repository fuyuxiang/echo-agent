import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { KnowledgePicker } from "../KnowledgePicker";
import { registerKbProvider, resetKbRegistry } from "@/lib/knowledge-base";
import { useKnowledgeStore } from "@/stores/knowledge-store";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";

const setKnowledgeSourcesMock = vi.hoisted(() => vi.fn());
vi.mock("@/lib/agent-client", () => ({
  agentSetKnowledgeSources: setKnowledgeSourcesMock,
}));

describe("KnowledgePicker", () => {
  it("菜单脱离裁切容器，支持方向键并在 Esc 后归还焦点", async () => {
    const { container } = render(<div style={{ overflow: "hidden" }}><KnowledgePicker /></div>);
    const trigger = screen.getByRole("button", { name: "知识来源，未选择" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const menu = screen.getByRole("menu", { name: "选择知识来源" });
    expect(container.contains(menu)).toBe(false);
    expect(trigger.getAttribute("aria-controls")).toBe(menu.id);
    const first = screen.getByRole("menuitemcheckbox", { name: /个人知识库/ });
    await waitFor(() => expect(first).toHaveFocus());
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(screen.getByRole("menuitemcheckbox", { name: /组织知识库/ })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape", isComposing: true });
    expect(menu).toBeInTheDocument();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("切换任务后旧同步完成不会解除新任务的加载状态", async () => {
    useKnowledgeStore.getState().setSourceCount(1);
    const resolvers: Array<(value: unknown) => void> = [];
    setKnowledgeSourcesMock.mockImplementation(() => new Promise(resolve => resolvers.push(resolve)));
    const { rerender } = render(<KnowledgePicker sessionId="a" />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    rerender(<KnowledgePicker sessionId="b" />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    await act(async () => { resolvers[0]({}); });
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "true");
    await act(async () => { resolvers[1]({}); });
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "false");
  });

  it("返回任务后旧同步失败不会覆盖新选择的组织范围", async () => {
    const onToast = vi.fn();
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [
            { id: "team-1", kind: "team", name: "产品团队" },
            { id: "org-1", kind: "org", name: "全公司" },
          ],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    useKnowledgeStore.setState({
      sessionSources: { a: ["organization"] },
      sessionOrganizationScopeIds: { a: [] },
    });
    const requests: Array<{ resolve: (value: unknown) => void; reject: (reason: unknown) => void }> = [];
    setKnowledgeSourcesMock.mockImplementation(() => new Promise((resolve, reject) => requests.push({ resolve, reject })));
    const { rerender } = render(<KnowledgePicker sessionId="a" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    fireEvent.change(screen.getByLabelText("组织知识范围"), { target: { value: "team-1" } });
    rerender(<KnowledgePicker sessionId="b" onToast={onToast} />);
    rerender(<KnowledgePicker sessionId="a" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    fireEvent.change(screen.getByLabelText("组织知识范围"), { target: { value: "org-1" } });
    expect(requests).toHaveLength(2);

    await act(async () => { requests[0].reject(new Error("旧范围同步超时")); });
    expect(useKnowledgeStore.getState().sessionSources.a).toEqual(["organization"]);
    expect(useKnowledgeStore.getState().sessionOrganizationScopeIds.a).toEqual(["org-1"]);
    expect(screen.getByLabelText("组织知识范围")).toHaveValue("org-1");
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "true");
    expect(onToast).not.toHaveBeenCalled();

    await act(async () => { requests[1].resolve({}); });
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "false");
    expect(useKnowledgeStore.getState().sessionOrganizationScopeIds.a).toEqual(["org-1"]);
  });

  it("切换任务后未被替代的旧同步失败仍回滚原任务", async () => {
    const onToast = vi.fn();
    useKnowledgeStore.getState().setSourceCount(1);
    const requests: Array<{ resolve: (value: unknown) => void; reject: (reason: unknown) => void }> = [];
    setKnowledgeSourcesMock.mockImplementation(() => new Promise((resolve, reject) => requests.push({ resolve, reject })));
    const { rerender } = render(<KnowledgePicker sessionId="a" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    rerender(<KnowledgePicker sessionId="b" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));

    await act(async () => { requests[0].reject(new Error("同步超时")); });
    expect(useKnowledgeStore.getState().sessionSources.a).toEqual([]);
    expect(useKnowledgeStore.getState().sessionSources.b).toEqual(["personal"]);
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "true");
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("已恢复上一选择"));

    await act(async () => { requests[1].resolve({}); });
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "false");
  });

  it.each([false, true])("卸载重开后旧实例拒绝不回滚新来源（新请求已完成：%s）", async (finishNewFirst) => {
    const onToast = vi.fn();
    useKnowledgeStore.getState().setSourceCount(1);
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [{ id: "team-1", kind: "team", name: "产品团队" }],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    const requests: Array<{ resolve: (value: unknown) => void; reject: (reason: unknown) => void }> = [];
    setKnowledgeSourcesMock.mockImplementation(() => new Promise((resolve, reject) => requests.push({ resolve, reject })));
    const first = render(<KnowledgePicker sessionId="remounted" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    first.unmount();

    render(<KnowledgePicker sessionId="remounted" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "个人知识" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /组织知识库/ }));
    expect(requests).toHaveLength(2);
    if (finishNewFirst) await act(async () => { requests[1].resolve({}); });
    await act(async () => { requests[0].reject(new Error("已卸载实例的同步超时")); });

    expect(useKnowledgeStore.getState().sessionSources.remounted).toEqual(["personal", "organization"]);
    expect(screen.getByRole("button", { name: "知识来源 2" })).toBeInTheDocument();
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", String(!finishNewFirst));
    expect(onToast).not.toHaveBeenCalled();
    if (!finishNewFirst) await act(async () => { requests[1].resolve({}); });
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "false");
  });

  it.each([
    { oldSucceeds: false, newFailsFirst: false },
    { oldSucceeds: true, newFailsFirst: false },
    { oldSucceeds: false, newFailsFirst: true },
    { oldSucceeds: true, newFailsFirst: true },
  ])("新请求失败恢复已确认来源（旧成功：$oldSucceeds，新失败先到：$newFailsFirst）", async ({ oldSucceeds, newFailsFirst }) => {
    const onToast = vi.fn();
    useKnowledgeStore.getState().setSourceCount(1);
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [{ id: "team-1", kind: "team", name: "产品团队" }],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    const requests: Array<{ resolve: (value: unknown) => void; reject: (reason: unknown) => void }> = [];
    setKnowledgeSourcesMock.mockImplementation(() => new Promise((resolve, reject) => requests.push({ resolve, reject })));
    const first = render(<KnowledgePicker sessionId="failed-chain" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    first.unmount();
    render(<KnowledgePicker sessionId="failed-chain" onToast={onToast} />);
    fireEvent.click(screen.getByRole("button", { name: "个人知识" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /组织知识库/ }));
    expect(requests).toHaveLength(2);
    const settleOld = () => oldSucceeds
      ? requests[0].resolve({})
      : requests[0].reject(new Error("旧请求失败"));
    const failNew = () => requests[1].reject(new Error("新请求失败"));
    if (newFailsFirst) {
      await act(async () => { failNew(); });
      await act(async () => { settleOld(); });
    } else {
      await act(async () => { settleOld(); });
      await act(async () => { failNew(); });
    }

    expect(useKnowledgeStore.getState().sessionSources["failed-chain"]).toEqual(oldSucceeds ? ["personal"] : []);
    expect(useKnowledgeStore.getState().sessionOrganizationScopeIds["failed-chain"] ?? []).toEqual([]);
    expect(screen.getByRole("button", { name: oldSucceeds ? "个人知识" : "知识来源，未选择" })).toBeInTheDocument();
    expect(screen.getByRole("menu")).toHaveAttribute("aria-busy", "false");
    expect(onToast).toHaveBeenCalledOnce();
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("新请求失败"));
  });

  it.each([false, true])("新范围失败恢复已确认范围（旧范围成功：%s）", async (oldSucceeds) => {
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [
            { id: "team-1", kind: "team", name: "产品团队" },
            { id: "org-1", kind: "org", name: "全公司" },
          ],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    useKnowledgeStore.setState({
      sessionSources: { "failed-scope": ["organization"] },
      sessionOrganizationScopeIds: { "failed-scope": [] },
    });
    const requests: Array<{ resolve: (value: unknown) => void; reject: (reason: unknown) => void }> = [];
    setKnowledgeSourcesMock.mockImplementation(() => new Promise((resolve, reject) => requests.push({ resolve, reject })));
    const first = render(<KnowledgePicker sessionId="failed-scope" />);
    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    fireEvent.change(screen.getByLabelText("组织知识范围"), { target: { value: "team-1" } });
    first.unmount();
    render(<KnowledgePicker sessionId="failed-scope" />);
    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    fireEvent.change(screen.getByLabelText("组织知识范围"), { target: { value: "org-1" } });
    await act(async () => {
      if (oldSucceeds) requests[0].resolve({});
      else requests[0].reject(new Error("旧范围失败"));
    });
    await act(async () => { requests[1].reject(new Error("新范围失败")); });

    expect(useKnowledgeStore.getState().sessionSources["failed-scope"]).toEqual(["organization"]);
    expect(useKnowledgeStore.getState().sessionOrganizationScopeIds["failed-scope"]).toEqual(oldSucceeds ? ["team-1"] : []);
    expect(screen.getByLabelText("组织知识范围")).toHaveValue(oldSucceeds ? "team-1" : "");
  });

  beforeEach(() => {
    resetKbRegistry();
    resetOrgSessionMirror();
    setKnowledgeSourcesMock.mockReset();
    setKnowledgeSourcesMock.mockResolvedValue({
      personalSelected: false,
      organizationSelected: false,
      personalAttached: false,
      organizationAttached: false,
    });
    useKnowledgeStore.setState({
      defaultSources: [],
      defaultOrganizationScopeIds: [],
      sessionSources: {},
      sessionOrganizationScopeIds: {},
      sourceCount: 0,
      retrievals: {},
      turnTraces: {},
    });
  });

  it("未配置个人知识时解释原因并进入管理页", () => {
    const onManage = vi.fn();
    render(<KnowledgePicker onManage={onManage} />);
    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    const personal = screen.getByRole("menuitemcheckbox", { name: /个人知识库/ });
    expect(personal).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(personal);
    expect(onManage).toHaveBeenCalledOnce();
  });

  it("支持为当前任务多选和取消知识来源", async () => {
    registerKbProvider({ id: "local", label: "本地：notes", isEnabled: () => true, list: () => [] });
    useKnowledgeStore.getState().setSourceCount(1);
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [{ id: "team-1", kind: "team", name: "产品团队" }],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    render(<KnowledgePicker sessionId="session-1" />);

    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    await waitFor(() => expect(screen.getByRole("menuitemcheckbox", { name: /组织知识库/ })).toBeEnabled());
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /组织知识库/ }));

    await waitFor(() => expect(useKnowledgeStore.getState().sessionSources["session-1"]).toEqual([
      "personal", "organization",
    ]));
    expect(screen.getByRole("button", { name: "知识来源 2" })).toBeInTheDocument();
    expect(setKnowledgeSourcesMock).toHaveBeenLastCalledWith("session-1", [
      "personal",
      "organization",
    ], []);

    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));
    await waitFor(() => expect(useKnowledgeStore.getState().sessionSources["session-1"])
      .toEqual(["organization"]));
  });

  it("原生同步失败时回滚勾选并告知用户", async () => {
    const onToast = vi.fn();
    registerKbProvider({ id: "local", label: "本地：notes", isEnabled: () => true, list: () => [] });
    useKnowledgeStore.getState().setSourceCount(1);
    setKnowledgeSourcesMock.mockRejectedValueOnce(new Error("MCP 连接超时"));
    render(<KnowledgePicker sessionId="session-failed" onToast={onToast} />);

    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /个人知识库/ }));

    await waitFor(() => expect(useKnowledgeStore.getState().sessionSources["session-failed"])
      .toEqual([]));
    expect(onToast).toHaveBeenCalledWith(expect.stringContaining("已恢复上一选择"));
  });

  it("组织未登录时不可选择，并提供明确入口", () => {
    const onOpenOrganization = vi.fn();
    useOrgSessionStore.setState({ hydrated: true, session: { loggedIn: false } });
    render(<KnowledgePicker onOpenOrganization={onOpenOrganization} />);

    fireEvent.click(screen.getByRole("button", { name: "知识来源，未选择" }));
    const organization = screen.getByRole("menuitemcheckbox", { name: /组织知识库/ });
    expect(organization).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByText("登录组织后可用")).toBeInTheDocument();
    fireEvent.click(organization);
    expect(useKnowledgeStore.getState().defaultSources).toEqual([]);
    fireEvent.click(screen.getByRole("button", { name: "登录组织" }));
    expect(onOpenOrganization).toHaveBeenCalledOnce();
  });

  it("可把当前任务的组织检索限定到单一授权范围", async () => {
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [
            { id: "team-1", kind: "team", name: "产品团队" },
            { id: "org-1", kind: "org", name: "全公司" },
          ],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    useKnowledgeStore.setState({
      sessionSources: { "session-scope": ["organization"] },
      sessionOrganizationScopeIds: { "session-scope": [] },
    });
    render(<KnowledgePicker sessionId="session-scope" />);

    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    fireEvent.change(screen.getByLabelText("组织知识范围"), { target: { value: "team-1" } });

    await waitFor(() => expect(useKnowledgeStore.getState()
      .sessionOrganizationScopeIds["session-scope"]).toEqual(["team-1"]));
    expect(setKnowledgeSourcesMock).toHaveBeenLastCalledWith(
      "session-scope",
      ["organization"],
      ["team-1"],
    );
    expect(screen.getByText("本任务只检索“产品团队”")).toBeInTheDocument();
  });

  it("已依赖组织知识的任务在登录失效后不会静默降级", () => {
    const onOpenOrganization = vi.fn();
    useOrgSessionStore.setState({ hydrated: true, session: { loggedIn: false } });
    useKnowledgeStore.setState({
      sessionSources: { stale: ["organization"] },
      sessionOrganizationScopeIds: { stale: ["team-1"] },
    });
    render(<KnowledgePicker sessionId="stale" onOpenOrganization={onOpenOrganization} />);

    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    expect(screen.getByRole("alert")).toHaveTextContent("当前任务仍依赖组织知识");
    fireEvent.click(screen.getByRole("button", { name: "重新登录" }));
    expect(onOpenOrganization).toHaveBeenCalledOnce();
    expect(useKnowledgeStore.getState().sessionSources.stale).toEqual(["organization"]);
  });

  it("原授权范围失效时不会在界面上伪装成检索全部范围", () => {
    useOrgSessionStore.setState({
      hydrated: true,
      session: {
        loggedIn: true,
        organizationMemoryEnabled: true,
        bootstrap: {
          apiVersion: 1,
          user: { id: "u1", username: "u1", displayName: "用户", role: "member", clearance: 1 },
          scopes: [{ id: "team-new", kind: "team", name: "新团队" }],
          policy: {},
          serverTime: Date.now(),
        },
      },
    });
    useKnowledgeStore.setState({
      sessionSources: { stale_scope: ["organization"] },
      sessionOrganizationScopeIds: { stale_scope: ["team-old"] },
    });
    render(<KnowledgePicker sessionId="stale_scope" />);

    fireEvent.click(screen.getByRole("button", { name: "组织知识" }));
    expect(screen.getByLabelText("组织知识范围")).toHaveValue("__invalid__");
    expect(screen.getByRole("alert")).toHaveTextContent("不会自动扩大检索范围");
    expect(screen.getByText("为避免越界检索，请明确选择新范围")).toBeInTheDocument();
  });
});
