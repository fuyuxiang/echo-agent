import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
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

describe("OrganizationMemoryPanel", () => {
  beforeEach(() => {
    resetOrgSessionMirror();
    for (const mock of Object.values(api)) mock.mockReset();
    for (const mock of Object.values(agentClientMocks)) mock.mockReset();
    agentClientMocks.filesystemPickFiles.mockResolvedValue([]);
    agentClientMocks.filesystemPickDirectory.mockResolvedValue(null);
    mockWorkspace();
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
    await screen.findByText("Alice · https://memory.example.com");
    expect(screen.getByRole("heading", { name: "组织知识" })).toBeInTheDocument();
    expect(screen.getByText("在对话中使用组织知识")).toBeInTheDocument();
    expect(screen.queryByText("组织问答")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "发起对话" }));
    expect(onStartConversation).toHaveBeenCalledOnce();
  });

  it("查看范围只过滤列表，不改变写入目标", async () => {
    render(<OrganizationMemoryPanel cwd="/workspace/payments" />);
    await screen.findByText("Alice · https://memory.example.com");
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
    await screen.findByText("Alice · https://memory.example.com");
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
    await screen.findByText("Alice · https://memory.example.com");
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    const view = screen.getAllByRole("button", { name: "查看" })[0];
    view.focus();
    fireEvent.click(view);
    const preview = await screen.findByRole("dialog", { name: /查看文档/ });
    expect(within(preview).getByRole("button", { name: "关闭预览" })).toHaveFocus();
    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(view).toHaveFocus();
    const publish = screen.getByRole("button", { name: "发布副本" });
    publish.focus();
    fireEvent.click(publish);
    const confirmation = screen.getByRole("dialog", { name: "发布文档副本" });
    const cancel = within(confirmation).getByRole("button", { name: "取消" });
    expect(cancel).toHaveFocus();
    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(publish).toHaveFocus();
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
    await screen.findByText("Alice · https://memory.example.com");
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "上传文档" }));
    const uploadDialog = screen.getByRole("dialog", { name: "选择上传位置" });
    fireEvent.change(within(uploadDialog).getByLabelText("文档上传范围"), { target: { value: teamScope.id } });
    fireEvent.click(within(uploadDialog).getByRole("button", { name: "取消" }));

    fireEvent.click(screen.getByRole("button", { name: "发布副本" }));
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
    await screen.findByText("Alice · https://memory.example.com");
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
    await screen.findByText("Alice · https://memory.example.com");
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
    expect(await screen.findByText("未加入共享组织范围")).toBeInTheDocument();
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
    await screen.findByText("Alice · https://memory.example.com");
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
    await screen.findByText("Alice · https://memory.example.com");
    fireEvent.click(screen.getByRole("button", { name: "退出组织" }));
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
    await screen.findByText("Alice · https://memory.example.com");
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
    await screen.findByText("Alice · https://memory.example.com");
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
    await screen.findByText("Alice · https://memory.example.com");
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
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
    await screen.findByText("Alice · https://memory.example.com");
    fireEvent.click(screen.getByRole("button", { name: /^文档/ }));
    fireEvent.click(screen.getByRole("button", { name: "上传文件夹" }));
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
});
