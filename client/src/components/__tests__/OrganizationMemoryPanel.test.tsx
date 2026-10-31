import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  orgLibraryQuery: vi.fn(), orgLibrarySummary: vi.fn(),
  orgSession: vi.fn(), orgLogin: vi.fn(), orgLogout: vi.fn(),
  orgMemoryPromotionsMine: vi.fn(), orgListScopes: vi.fn(), orgListDocuments: vi.fn(),
  orgListMemories: vi.fn(), orgListSkills: vi.fn(), orgDocumentSubmissionsMinePage: vi.fn(),
  orgSkillSubmissionsMine: vi.fn(), orgSubmitDocument: vi.fn(), orgSubmitMemoryCandidate: vi.fn(),
  orgArchiveDocument: vi.fn(), orgNewDocumentVersion: vi.fn(), orgPublishDocument: vi.fn(),
  orgSetSkillPreference: vi.fn(), orgPublishSkill: vi.fn(), orgSubmitSkill: vi.fn(), orgSyncSkills: vi.fn(),
  orgDownloadDocument: vi.fn(), orgFetchDocument: vi.fn(), orgPreviewDocument: vi.fn(),
  orgRemoveOwnSubmission: vi.fn(), orgScanDocumentFolder: vi.fn(),
}));
vi.mock("@/lib/org-client", () => api);
const agentClientMocks = vi.hoisted(() => ({ filesystemPickFiles: vi.fn(async () => []), filesystemPickDirectory: vi.fn(async () => null) }));
vi.mock("@/lib/agent-client", () => agentClientMocks);

import { OrganizationMemoryPanel } from "../OrganizationMemoryPanel";
import { resetOrgSessionMirror, useOrgSessionStore } from "@/stores/org-session-store";
import { filesystemPickDirectory, filesystemPickFiles } from "@/lib/agent-client";

const personalScope = { id: "personal-scope", kind: "personal" as const, name: "我的空间" };
const teamScope = { id: "team-scope", kind: "team" as const, name: "研发团队" };

function document(id: string, title: string, scopeId: string, scopeName: string) {
  return {
    id, title, sourceType: "md", status: "ready" as const, byteSize: 128, scopeId, ownerId: "u1",
    scopeKind: scopeId === personalScope.id ? "personal" as const : "team" as const,
    scopeName, chunkCount: 1, tags: [], updatedAt: 1,
  };
}

function mockWorkspace() {
  api.orgSession.mockResolvedValue({
    loggedIn: true, organizationMemoryEnabled: true, serverUrl: "https://memory.example.com",
    user: { id: "u1", username: "alice", displayName: "Alice", role: "member", clearance: 1 },
    bootstrap: {
      apiVersion: 1,
      user: { id: "u1", username: "alice", displayName: "Alice", role: "member", clearance: 1 },
      scopes: [personalScope, teamScope], policy: { allowSkillSubmission: true, allowPersonalCloud: true }, serverTime: 1,
    },
  });
  api.orgListScopes.mockResolvedValue([personalScope, teamScope]);
  api.orgListDocuments.mockImplementation(async (scopeId?: string) => {
    const items = [document("d1", "个人文档", personalScope.id, personalScope.name), document("d2", "团队文档", teamScope.id, teamScope.name)]
      .filter((item) => !scopeId || item.scopeId === scopeId);
    return { items, total: items.length, page: 1, size: 20 };
  });
  api.orgListSkills.mockResolvedValue([]);
  api.orgListMemories.mockResolvedValue([]);
  api.orgMemoryPromotionsMine.mockResolvedValue([]);
  api.orgDocumentSubmissionsMinePage.mockResolvedValue({ items: [], total: 0, page: 1, size: 20 });
  api.orgSkillSubmissionsMine.mockResolvedValue([]);
  api.orgLogout.mockResolvedValue(undefined);
  api.orgSubmitMemoryCandidate.mockResolvedValue({ promotionId: "p1", state: "pending" });
}

function documentAction(name: string, index = 0) {
  const trigger = screen.getAllByRole("button", { name: /更多文档操作/ })[index];
  fireEvent.click(trigger);
  fireEvent.click(screen.getByRole("menuitem", { name }));
  return trigger;
}

describe("OrganizationMemoryPanel", () => {
  beforeEach(() => {
    resetOrgSessionMirror();
    for (const mock of Object.values(api)) mock.mockReset();
    for (const mock of Object.values(agentClientMocks)) mock.mockReset();
    agentClientMocks.filesystemPickFiles.mockResolvedValue([]);
    agentClientMocks.filesystemPickDirectory.mockResolvedValue(null);
    mockWorkspace();
    api.orgLibraryQuery.mockImplementation(async (resource: string, query: {scopeId:string;query:string;filter:string;page:number;size:number}) => {
      let items: any[] = [];
      let total: number | undefined;
      if(resource === "documents") { const page = await api.orgListDocuments(query.scopeId || undefined, query.query || undefined, query.page, query.size); items=page.items; total=page.total; }
      else if(resource === "document-submissions") { const page=await api.orgDocumentSubmissionsMinePage(query.page,query.size);items=page.items;total=page.total; }
      else if(resource === "memories") items=await api.orgListMemories();
      else if(resource === "skills") items=await api.orgListSkills();
      else if(resource === "memory-submissions") items=(await api.orgMemoryPromotionsMine()).filter((item:any)=>item.payloadType === "memory");
      else items=await api.orgSkillSubmissionsMine();
      items=items.filter(item=>(!query.scopeId || item.scopeId===query.scopeId) && (!query.query || JSON.stringify(item).includes(query.query)));
      return {items: total === undefined ? items.slice((query.page-1)*query.size,query.page*query.size):items,total:total??items.length,page:query.page,size:query.size};
    });
    api.orgLibrarySummary.mockImplementation(async () => {
      await Promise.resolve();
      const result = async (mock: ReturnType<typeof vi.fn>) => await mock.mock.results[mock.mock.results.length - 1]?.value;
      const [docs, memories, skills, promos, docSubs, skillSubs] = await Promise.all([api.orgListDocuments,api.orgListMemories,api.orgListSkills,api.orgMemoryPromotionsMine,api.orgDocumentSubmissionsMinePage,api.orgSkillSubmissionsMine].map(result));
      return {memories:memories?.length??0,documents:docs?.total??0,documentReady:docs?.items?.filter((item:any)=>item.status === "ready").length??0,documentProcessing:docs?.items?.filter((item:any)=>["pending","parsing","chunking","embedding"].includes(item.status)).length??0,
        skills:skills?.length??0,skillInstalled:skills?.filter((item:any)=>item.enabled).length??0,skillMandatory:skills?.filter((item:any)=>item.mandatory).length??0,
        memoryPending:promos?.filter((item:any)=>item.state === "pending" && item.payloadType === "memory").length??0,documentPending:docSubs?.items?.filter((item:any)=>item.state === "pending").length??0,skillPending:skillSubs?.filter((item:any)=>item.state === "pending").length??0};
    });
  });

  it("首次加载失败不显示零数据，重试后恢复真实资源数量", async () => {
    api.orgListScopes.mockRejectedValueOnce(new Error("error sending request for url (https://memory.example.com/api/v1/auth/refresh)"));
    render(<OrganizationMemoryPanel />);
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时无法连接组织服务");
    expect(screen.queryByLabelText("组织知识概况")).toBeNull();
    expect(screen.getByText("组织资料暂不可用，恢复连接后刷新查看。")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByLabelText("组织知识概况")).toHaveTextContent("可检索文档");
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(api.orgListScopes).toHaveBeenCalledTimes(2);
  });

  it("刷新失败保留已加载内容，明确提示资料未更新", async () => {
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    api.orgListScopes.mockRejectedValueOnce(new Error("network connection failed"));
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时无法连接组织服务");
    expect(screen.getByText("组织资料更新失败，下方保留上次加载的内容。")).toBeInTheDocument();
    expect(screen.getByLabelText("组织知识概况")).toHaveTextContent("可检索文档");
  });

  it("登录凭据失效后回填服务器和账号，只要求重新输入密码", async () => {
    api.orgSession.mockResolvedValue({
      loggedIn: false,
      organizationMemoryEnabled: false,
      serverUrl: "https://memory.example.com",
      username: "alice",
      requiresReauthentication: true,
    });

    render(<OrganizationMemoryPanel />);

    expect(await screen.findByLabelText("服务器地址")).toHaveValue("https://memory.example.com");
    expect(screen.getByLabelText("账号")).toHaveValue("alice");
    expect(screen.getByLabelText("密码")).toHaveValue("");
    expect(screen.getByText("登录状态已过期，服务器和账号已为你保留，请重新输入密码。")).toBeInTheDocument();
    expect(api.orgListScopes).not.toHaveBeenCalled();
  });

  it("登录后进入管理概览且不再提供独立组织问答", async () => {
    const onStartConversation = vi.fn();
    render(<OrganizationMemoryPanel onStartConversation={onStartConversation} />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    expect(screen.getByRole("heading", { name: "组织知识" })).toBeInTheDocument();
    expect(screen.getByText("在对话中使用组织知识")).toBeInTheDocument();
    expect(screen.queryByText("组织问答")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "发起对话" }));
    expect(onStartConversation).toHaveBeenCalledOnce();
  });

  it("原生登录事件先于 IPC 返回时仍加载工作区及写入范围", async () => {
    const signedIn = await api.orgSession();
    api.orgSession.mockResolvedValue({ loggedIn: false, serverUrl: signedIn.serverUrl, username: "alice" });
    let completeLogin!: (session: typeof signedIn) => void;
    api.orgLogin.mockReturnValue(new Promise((resolve) => { completeLogin = resolve; }));
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("密码");
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "test-password" } });
    fireEvent.click(screen.getByRole("button", { name: "安全登录" }));
    act(() => useOrgSessionStore.getState().setSession(signedIn));
    await waitFor(() => expect(api.orgListDocuments).toHaveBeenCalled());
    await act(async () => completeLogin(signedIn));
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    expect(await screen.findByText("团队文档")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "上传文档" })).toBeEnabled();
  });

  it("登录返回前已退出时，迟到的成功结果不能重新登录或恢复旧文档", async () => {
    const signedIn = await api.orgSession();
    api.orgSession.mockResolvedValue({ loggedIn: false, serverUrl: signedIn.serverUrl, username: "alice" });
    let completeLogin!: (session: typeof signedIn) => void;
    api.orgLogin.mockReturnValue(new Promise((resolve) => { completeLogin = resolve; }));
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("密码");
    fireEvent.change(screen.getByLabelText("密码"), { target: { value: "test-password" } });
    fireEvent.click(screen.getByRole("button", { name: "安全登录" }));
    act(() => {
      useOrgSessionStore.getState().setSession(signedIn);
      useOrgSessionStore.getState().clearSession();
    });
    await act(async () => completeLogin(signedIn));
    expect(await screen.findByLabelText("密码")).toBeInTheDocument();
    expect(screen.queryByText("团队文档")).toBeNull();
    expect(useOrgSessionStore.getState().session?.loggedIn).toBe(false);
  });

  it("账号切换后丢弃旧账号的迟到列表和预览", async () => {
    let resolvePreview!: (value: { kind: "text"; content: string }) => void;
    api.orgPreviewDocument.mockReturnValue(new Promise((resolve) => { resolvePreview = resolve; }));
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getAllByRole("button", { name: "查看" })[0]);

    api.orgListDocuments.mockResolvedValue({ items: [], total: 0, page: 1, size: 20 });
    act(() => useOrgSessionStore.getState().setSession({
      loggedIn: true, serverUrl: "https://other.example.com",
      user: { id: "u2", username: "bob", displayName: "Bob", role: "member", clearance: 0 },
    }));
    await screen.findByRole("button", { name: "组织账号：Bob" });
    await act(async () => resolvePreview({ kind: "text", content: "Alice 私有文档内容" }));

    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    expect(screen.queryByText("个人文档")).toBeNull();
    expect(screen.queryByText("Alice 私有文档内容")).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("退出后丢弃迟到的工作区恢复，重新登录失败不展示旧文档", async () => {
    let resolveDocuments!: (value: { items: ReturnType<typeof document>[]; total: number; page: number; size: number }) => void;
    api.orgListDocuments.mockReturnValue(new Promise((resolve) => { resolveDocuments = resolve; }));
    render(<OrganizationMemoryPanel />);
    await waitFor(() => expect(api.orgListDocuments).toHaveBeenCalledOnce());
    act(() => useOrgSessionStore.getState().clearSession());
    await screen.findByLabelText("密码");
    await act(async () => resolveDocuments({
      items: [document("old", "旧账号保密文档", personalScope.id, personalScope.name)],
      total: 1, page: 1, size: 20,
    }));
    api.orgListScopes.mockRejectedValueOnce(new Error("新账号组织服务不可达"));
    act(() => useOrgSessionStore.getState().setSession({
      loggedIn: true, serverUrl: "https://new.example.com",
      user: { id: "u2", username: "bob", displayName: "Bob", role: "member", clearance: 0 },
    }));
    await screen.findByRole("button", { name: "组织账号：Bob" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    expect(screen.queryByText("旧账号保密文档")).toBeNull();
  });

  it("更新文档原件选择失败时显示恢复提示", async () => {
    agentClientMocks.filesystemPickFiles.mockRejectedValueOnce(new Error("文件选择器不可用"));
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    documentAction("新版本");
    expect(await screen.findByText("选择新版本失败：文件选择器不可用")).toBeInTheDocument();
    expect(api.orgNewDocumentVersion).not.toHaveBeenCalled();
  });

  it("查看范围只过滤列表，不改变写入目标", async () => {
    render(<OrganizationMemoryPanel cwd="/workspace/payments" />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^经验/ }));
    fireEvent.change(screen.getByLabelText("查看范围"), { target: { value: teamScope.id } });
    fireEvent.click(screen.getByRole("button", { name: "提交经验" }));
    expect(screen.getByLabelText("发布范围")).toHaveValue(personalScope.id);
    fireEvent.change(screen.getByLabelText("发布范围"), { target: { value: teamScope.id } });
    fireEvent.change(screen.getByPlaceholderText("写成可直接指导下一次任务的明确结论或步骤"), { target: { value: "发布前先验证回滚脚本" } });
    fireEvent.click(screen.getByRole("button", { name: "提交审核" }));
    await waitFor(() => expect(api.orgSubmitMemoryCandidate).toHaveBeenCalledWith(expect.objectContaining({
      targetScopeId: teamScope.id, content: "发布前先验证回滚脚本", workspaceRef: "/workspace/payments",
    })));
  });

  it("切换查看范围时只显示当前范围文档", async () => {
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    expect(screen.getByText("个人文档")).toBeInTheDocument();
    expect(screen.getByText("团队文档")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("查看范围"), { target: { value: teamScope.id } });
    await waitFor(() => expect(screen.queryByText("个人文档")).not.toBeInTheDocument());
    expect(screen.getByText("团队文档")).toBeInTheDocument();
  });

  it("文档预览、发布与移除圈定焦点，Escape 恢复入口且不执行动作", async () => {
    api.orgPreviewDocument.mockResolvedValue({ kind: "text", content: "# 文档内容" });
    api.orgDocumentSubmissionsMinePage.mockResolvedValue({
      items: [{ id: "focus-submission", title: "待移除文档", sourceType: "md", byteSize: 128,
        scopeId: teamScope.id, scopeName: teamScope.name, scopeKind: teamScope.kind,
        state: "approved", scanStatus: "passed", resultDocumentId: "d2", createdAt: 1 }],
      total: 1, page: 1, size: 20,
    });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    const view = screen.getAllByRole("button", { name: "查看" })[0];
    view.focus();
    fireEvent.click(view);
    const preview = await screen.findByRole("dialog", { name: /查看文档/ });
    expect(within(preview).getByRole("button", { name: "关闭预览" })).toHaveFocus();
    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(view).toHaveFocus();
    const publish = documentAction("发布副本");
    const confirmation = screen.getByRole("dialog", { name: "发布文档副本" });
    const cancel = within(confirmation).getByRole("button", { name: "取消" });
    expect(cancel).toHaveFocus();
    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(publish).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "我的提交" }));
    const remove = screen.getAllByRole("button", { name: "删除" })[0];
    remove.focus();
    fireEvent.click(remove);
    const removeDialog = screen.getByRole("alertdialog", { name: "确认删除文档" });
    expect(within(removeDialog).getByRole("button", { name: "取消" })).toHaveFocus();
    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(remove).toHaveFocus();
    expect(api.orgPublishDocument).not.toHaveBeenCalled();
    expect(api.orgArchiveDocument).not.toHaveBeenCalled();
  });

  it("写入范围为团队时仍可为个人文档选择副本发布目标", async () => {
    api.orgPublishDocument.mockResolvedValue({ state: "approved" });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "上传文档" }));
    const uploadDialog = screen.getByRole("dialog", { name: "选择上传位置" });
    fireEvent.change(within(uploadDialog).getByLabelText("文档上传范围"), { target: { value: teamScope.id } });
    fireEvent.click(within(uploadDialog).getByRole("button", { name: "取消" }));

    documentAction("发布副本");
    const dialog = screen.getByRole("dialog", { name: "发布文档副本" });
    expect(within(dialog).getByLabelText("文档副本发布目标")).toHaveValue(teamScope.id);
    fireEvent.click(within(dialog).getByRole("button", { name: "确认发布" }));
    await waitFor(() => expect(api.orgPublishDocument).toHaveBeenCalledWith("d1", teamScope.id));
  });

  it.each(["docx", "xlsx", "pptx"])("%s 文档优先显示服务端解析内容", async (sourceType) => {
    api.orgListDocuments.mockResolvedValue({
      items: [{ ...document("office-1", `示例.${sourceType}`, teamScope.id, teamScope.name), sourceType }],
      total: 1, page: 1, size: 20,
    });
    api.orgFetchDocument.mockResolvedValue({ docId: "office-1", text: "文档中的实际内容", chunks: [{ seq: 0, text: "文档中的实际内容" }] });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "查看" }));
    expect(await screen.findByText("文档中的实际内容")).toBeInTheDocument();
    expect(api.orgFetchDocument).toHaveBeenCalledWith("office-1", null, "0:199");
    expect(api.orgPreviewDocument).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "下载原件" })).toBeInTheDocument();
  });

  it("Office 文档尚未解析时在预览窗口提供下载", async () => {
    api.orgListDocuments.mockResolvedValue({
      items: [{ ...document("office-2", "待解析.docx", teamScope.id, teamScope.name), sourceType: "docx" }],
      total: 1, page: 1, size: 20,
    });
    api.orgFetchDocument.mockResolvedValue({ docId: "office-2", text: "", chunks: [] });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "查看" }));
    const dialog = await screen.findByRole("dialog", { name: "查看文档 待解析.docx" });
    expect(within(dialog).getByText("暂时无法在线预览")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "下载原件" })).toBeInTheDocument();
    expect(api.orgPreviewDocument).not.toHaveBeenCalled();
  });

  it("组织上下文不可用时禁用发起对话但保留管理页", async () => {
    api.orgSession.mockResolvedValue({
      loggedIn: true, organizationMemoryEnabled: false, serverUrl: "https://memory.example.com",
      user: { id: "u1", username: "alice", displayName: "Alice", role: "member", clearance: 1 },
      bootstrap: { apiVersion: 1, user: { id: "u1", username: "alice", displayName: "Alice", role: "member", clearance: 1 }, scopes: [personalScope], policy: {}, serverTime: 1 },
    });
    api.orgListScopes.mockResolvedValue([personalScope]);
    render(<OrganizationMemoryPanel onStartConversation={vi.fn()} />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    expect(screen.getAllByText("暂无团队或组织共享权限").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "发起对话" })).toBeDisabled();
  });

  it("组织 Skill 可以安装到当前设备", async () => {
    api.orgListSkills.mockResolvedValue([{
      skillId: "skill-1", versionId: "version-1", name: "shared-guide", description: "共享指南",
      version: "1.0.0", scopeId: personalScope.id, scopeKind: "personal", scopeName: personalScope.name,
      mandatory: false, allowPersonalOverride: true, enabled: false, updatedAt: 1,
    }]);
    api.orgSetSkillPreference.mockResolvedValue({ skillId: "skill-1", enabled: true });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^Skills/ }));
    fireEvent.click(await screen.findByRole("button", { name: "安装" }));
    await waitFor(() => expect(api.orgSetSkillPreference).toHaveBeenCalledWith("skill-1", true));
  });

  it("概览按待处理类型展示并导航到正确工作区", async () => {
    api.orgMemoryPromotionsMine.mockResolvedValue([{
      id: "memory-pending",
      payloadType: "memory",
      payload: { kind: "howto", content: "上线前检查回滚" },
      source: "conversation",
      state: "pending",
      scopeName: teamScope.name,
      scopeKind: teamScope.kind,
      createdAt: 1,
    }]);
    api.orgDocumentSubmissionsMinePage.mockResolvedValue({ items: [{
      id: "document-pending",
      title: "发布手册",
      state: "pending",
      scopeId: teamScope.id,
      scopeName: teamScope.name,
      scopeKind: teamScope.kind,
      createdAt: 1,
    }], total: 1, page: 1, size: 20 });
    api.orgSkillSubmissionsMine.mockResolvedValue([{
      id: "skill-pending",
      name: "release-check",
      state: "pending",
      scopeId: teamScope.id,
      scopeName: teamScope.name,
      scopeKind: teamScope.kind,
      createdAt: 1,
    }]);

    render(<OrganizationMemoryPanel />);
    expect(await screen.findByText("1 条经验待审核")).toBeInTheDocument();
    expect(screen.getByText("1 项文档正在扫描或建立索引")).toBeInTheDocument();
    expect(screen.getByText("1 个 Skill 正在审核或扫描")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "查看经验" }));
    expect(screen.getByRole("heading", { name: "经验" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "概览" }));
    fireEvent.click(screen.getByRole("button", { name: "查看文档" }));
    expect(screen.getByRole("heading", { name: "文档" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "概览" }));
    fireEvent.click(screen.getByRole("button", { name: "查看 Skills" }));
    expect(screen.getByRole("heading", { name: "组织 Skills" })).toBeInTheDocument();
  });

  it("经验或 Skill 待审核时持续刷新工作区状态", async () => {
    vi.useFakeTimers();
    try {
      api.orgMemoryPromotionsMine.mockResolvedValue([{
        id: "memory-polling",
        payloadType: "memory",
        payload: { kind: "fact", content: "需要等待审核" },
        source: "conversation",
        state: "pending",
        scopeName: teamScope.name,
        scopeKind: teamScope.kind,
        createdAt: 1,
      }]);
      render(<OrganizationMemoryPanel />);
      await act(async () => { await Promise.resolve(); });
      expect(api.orgMemoryPromotionsMine).toHaveBeenCalledTimes(1);

      await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
      expect(api.orgMemoryPromotionsMine).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("注销请求未返回时立即隐藏旧组织数据", async () => {
    let finishLogout: (() => void) | undefined;
    api.orgLogout.mockImplementation(() => new Promise<void>((resolve) => { finishLogout = resolve; }));
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: "组织账号：Alice" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "退出登录" }));
    expect(screen.getByRole("heading", { name: "连接组织" })).toBeInTheDocument();
    expect(screen.queryByText("Alice · https://memory.example.com")).not.toBeInTheDocument();
    expect(useOrgSessionStore.getState().session?.loggedIn).toBe(false);
    expect(useOrgSessionStore.getState().session).toMatchObject({
      serverUrl: "https://memory.example.com",
      username: "alice",
      requiresReauthentication: false,
    });
    expect(screen.getByLabelText("服务器地址")).toHaveValue("https://memory.example.com");
    expect(screen.getByLabelText("账号")).toHaveValue("alice");
    await act(async () => finishLogout?.());
  });

  it("文档批量上传限制为三个并发任务", async () => {
    let active = 0;
    let peak = 0;
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    api.orgSubmitDocument.mockImplementation(async (path: string) => {
      active += 1;
      peak = Math.max(peak, active);
      await gate;
      active -= 1;
      return { id: path, state: "pending" };
    });
    vi.mocked(filesystemPickFiles).mockResolvedValue(Array.from({ length: 5 }, (_, index) => `/tmp/${index}.md`));
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "上传文档" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "选择上传位置" })).getByRole("button", { name: "选择文件" }));
    await waitFor(() => expect(api.orgSubmitDocument).toHaveBeenCalledTimes(3));
    expect(peak).toBe(3);
    await act(async () => release?.());
    await waitFor(() => expect(api.orgSubmitDocument).toHaveBeenCalledTimes(5));
    expect(peak).toBeLessThanOrEqual(3);
  });

  it("重复文档在批量上传结果中计为跳过", async () => {
    const toast = vi.fn();
    api.orgSubmitDocument.mockImplementation(async (path: string) => path.endsWith("0.md")
      ? { state: "duplicate", dedup: true }
      : { state: "approved", dedup: false });
    vi.mocked(filesystemPickFiles).mockResolvedValue(["/tmp/0.md", "/tmp/1.md"]);
    render(<OrganizationMemoryPanel onToast={toast} />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "上传文档" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "选择上传位置" })).getByRole("button", { name: "选择文件" }));
    await waitFor(() => expect(toast).toHaveBeenCalledWith(expect.stringContaining("新增 1 个文档，跳过重复 1 个")));
  });

  it("已提交文档可预览、下载并由本人确认移除", async () => {
    api.orgDocumentSubmissionsMinePage.mockResolvedValue({
      items: [{ id: "submission-1", title: "报告", sourceType: "md", byteSize: 128,
        scopeId: teamScope.id, scopeName: teamScope.name, scopeKind: teamScope.kind,
        state: "approved", scanStatus: "passed", resultDocumentId: "d2", createdAt: 1 }],
      total: 1, page: 1, size: 20,
    });
    api.orgPreviewDocument.mockResolvedValue({ kind: "text", content: "# 原件内容" });
    api.orgDownloadDocument.mockResolvedValue("/tmp/report.md");
    api.orgRemoveOwnSubmission.mockResolvedValue({ removed: true });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "我的提交" }));
    const row = screen.getByText("报告").closest(".org-document-submissions__row") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "查看" }));
    expect(await screen.findByRole("dialog", { name: "查看文档 报告.md" })).toBeInTheDocument();
    expect(api.orgPreviewDocument).toHaveBeenCalledWith("submission-1", true, "md", "报告.md");
    fireEvent.click(screen.getByRole("button", { name: "关闭预览" }));
    fireEvent.click(within(row).getByRole("button", { name: "下载" }));
    await waitFor(() => expect(api.orgDownloadDocument).toHaveBeenCalledWith("submission-1", true, "报告.md"));
    fireEvent.click(within(row).getByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "确认移除" }));
    await waitFor(() => expect(api.orgRemoveOwnSubmission).toHaveBeenCalledWith("submission-1"));
  });

  it("文件夹上传先展示文件清单，保留相对路径作为文档名", async () => {
    vi.mocked(filesystemPickDirectory).mockResolvedValue("/workspace/docs");
    api.orgScanDocumentFolder.mockResolvedValue({
      items: [
        { path: "/workspace/docs/a.md", relativePath: "a.md", size: 10 },
        { path: "/workspace/docs/nested/b.txt", relativePath: "nested/b.txt", size: 20 },
      ], skipped: 1, totalBytes: 30,
    });
    api.orgSubmitDocument.mockResolvedValue({ state: "approved" });
    render(<OrganizationMemoryPanel />);
    await screen.findByRole("button", { name: "组织账号：Alice" });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "更多上传方式" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "上传文件夹" }));
    const uploadDialog = screen.getByRole("dialog", { name: "选择上传位置" });
    fireEvent.change(within(uploadDialog).getByLabelText("文档上传范围"), { target: { value: teamScope.id } });
    fireEvent.click(within(uploadDialog).getByRole("button", { name: "选择文件夹" }));
    expect(await screen.findByText("准备上传 2 个文档")).toBeInTheDocument();
    expect(screen.getByText(/目标：研发团队/)).toBeInTheDocument();
    expect(api.orgSubmitDocument).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "开始上传" }));
    await waitFor(() => expect(api.orgSubmitDocument).toHaveBeenCalledWith(
      "/workspace/docs/nested/b.txt", teamScope.id, "nested/b.txt",
    ));
  });
  it("概览与左侧显示服务端总数，翻页与刷新不丢失当前页", async () => {
    api.orgLibrarySummary.mockResolvedValue({ memories: 245, documents: 126, documentReady: 120, documentProcessing: 6, skills: 67, skillInstalled: 12, skillMandatory: 3, memoryPending: 0, documentPending: 0, skillPending: 0 });
    const original = api.orgLibraryQuery.getMockImplementation()!;
    api.orgLibraryQuery.mockImplementation(async (resource, query) => resource === "documents"
      ? { items: [document(`page-${query.page}`, `第 ${query.page} 页文档`, teamScope.id, teamScope.name)], total: 126, page: query.page, size: query.size }
      : original(resource, query));
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    expect(screen.getByRole("button", { name: "文档 126" })).toBeInTheDocument();
    expect(screen.getByLabelText("组织知识概况")).toHaveTextContent("120");
    fireEvent.click(screen.getByRole("button", { name: "文档 126" }));
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("第 2 页文档")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "刷新" })).toBeEnabled());
    expect(screen.getByText("第 2 页文档")).toBeInTheDocument();
    expect(screen.getByText("2 / 7")).toBeInTheDocument();
  });

  it("三个资源及我的提交分别保存筛选条件，不互相改变", async () => {
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    fireEvent.click(screen.getByRole("button", { name: /^经验/ }));
    fireEvent.change(screen.getByLabelText("查看范围"), { target: { value: teamScope.id } });
    fireEvent.change(screen.getByLabelText("经验类型"), { target: { value: "howto" } });
    fireEvent.click(screen.getByRole("button", { name: "我的提交" }));
    expect(screen.getByLabelText("查看范围")).toHaveValue("");
    fireEvent.change(screen.getByLabelText("提交状态"), { target: { value: "approved" } });
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    expect(screen.getByLabelText("查看范围")).toHaveValue("");
    fireEvent.click(screen.getByRole("button", { name: /^经验/ }));
    expect(screen.getByLabelText("提交状态")).toHaveValue("approved");
    fireEvent.click(screen.getByRole("button", { name: "已发布" }));
    expect(screen.getByLabelText("查看范围")).toHaveValue(teamScope.id);
    expect(screen.getByLabelText("经验类型")).toHaveValue("howto");
  });

  it("迟到的旧搜索不能覆盖新搜索结果", async () => {
    let finishOld!: (value: unknown) => void;
    const original = api.orgLibraryQuery.getMockImplementation()!;
    api.orgLibraryQuery.mockImplementation(async (resource, query) => {
      if (resource !== "documents" || !query.query) return original(resource, query);
      if (query.query === "旧搜索") return new Promise(resolve => { finishOld = resolve; });
      return { items: [document("new", "新搜索结果", teamScope.id, teamScope.name)], total: 1, page: 1, size: 20 };
    });
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "旧搜索" } });
    await waitFor(() => expect(finishOld).toBeTypeOf("function"));
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "新搜索" } });
    expect(await screen.findByText("新搜索结果")).toBeInTheDocument();
    await act(async () => finishOld({ items: [document("old", "旧搜索结果", teamScope.id, teamScope.name)], total: 1, page: 1, size: 20 }));
    expect(screen.queryByText("旧搜索结果")).toBeNull();
    expect(screen.getByText("新搜索结果")).toBeInTheDocument();
  });

  it("我的提交显示完整分页，不再截断为8条", async () => {
    api.orgMemoryPromotionsMine.mockResolvedValue(Array.from({ length: 31 }, (_, index) => ({ id: `p-${index}`, payloadType: "memory", payload: { content: `经验记录 ${index+1}` }, state: "approved", scopeId: teamScope.id, scopeName: teamScope.name, scopeKind: "team" })));
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    fireEvent.click(screen.getByRole("button", { name: /^经验/ }));
    fireEvent.click(screen.getByRole("button", { name: "我的提交" }));
    expect(screen.getByText("经验记录 20")).toBeInTheDocument();
    expect(screen.queryByText("经验记录 21")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("经验记录 31")).toBeInTheDocument();
    expect(screen.queryByText("经验记录 1")).toBeNull();
  });

  it("经验弹窗关闭后保留草稿并恢复提交入口焦点", async () => {
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    fireEvent.click(screen.getByRole("button", { name: /^经验/ }));
    const trigger=screen.getByRole("button", { name: "提交经验" });
    trigger.focus(); fireEvent.click(trigger);
    fireEvent.change(screen.getByLabelText("经验内容"), { target: { value: "发布前检查回滚脚本" } });
    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(trigger).toHaveFocus();
    fireEvent.click(trigger);
    expect(screen.getByLabelText("经验内容")).toHaveValue("发布前检查回滚脚本");
  });

  it("新筛选失败不会把旧范围结果冒充当前结果，重试可恢复", async () => {
    const original=api.orgLibraryQuery.getMockImplementation()!;
    let failed=false;
    api.orgLibraryQuery.mockImplementation(async(resource, query)=>{
      if(resource==="documents"&&query.scopeId===teamScope.id&&!failed){failed=true;throw new Error("network failed");}
      return original(resource,query);
    });
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.change(screen.getByLabelText("查看范围"), { target: { value: teamScope.id } });
    await screen.findByRole("alert");
    expect(screen.queryByText("个人文档")).toBeNull();
    expect(screen.queryByText("当前范围暂无已发布内容")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("团队文档")).toBeInTheDocument();
  });

  it("Skill共享目标只在操作弹窗中选择，提交范围与查看范围分离", async()=>{
    render(<OrganizationMemoryPanel />);
    await screen.findByLabelText("组织知识概况");
    fireEvent.click(screen.getByRole("button", { name: /^Skills/ }));
    expect(screen.queryByLabelText("Skill 提交范围")).toBeNull();
    fireEvent.change(screen.getByLabelText("查看范围"),{target:{value:teamScope.id}});
    fireEvent.click(screen.getByRole("button", { name: "提交 Skill" }));
    const dialog=screen.getByRole("dialog",{name:"提交 Skill"});
    expect(within(dialog).getByLabelText("Skill 提交范围")).toHaveValue(personalScope.id);
    fireEvent.click(within(dialog).getByRole("button",{name:"取消"}));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

});
