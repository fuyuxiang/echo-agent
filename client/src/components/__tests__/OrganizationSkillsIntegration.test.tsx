import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";

const mocks = vi.hoisted(() => ({
  skillsList: vi.fn(),
  skillsToggle: vi.fn(),
  internalReload: vi.fn(),
  orgSetSkillPreference: vi.fn(),
  listenOrgSkillsChanged: vi.fn(),
  orgSession: vi.fn(),
  orgListScopes: vi.fn(),
  orgSubmitSkill: vi.fn(),
}));

vi.mock("@/lib/agent-client", () => ({
  skillsList: mocks.skillsList,
  skillsRemove: vi.fn(),
  skillsToggle: mocks.skillsToggle,
  internalReload: mocks.internalReload,
  skillsUninstallPackage: vi.fn(),
}));
vi.mock("@/lib/org-client", () => ({
  orgSetSkillPreference: mocks.orgSetSkillPreference,
  listenOrgSkillsChanged: mocks.listenOrgSkillsChanged,
  orgSession: mocks.orgSession,
  orgListScopes: mocks.orgListScopes,
  orgSubmitSkill: mocks.orgSubmitSkill,
}));
vi.mock("../experts-panel/skills/ImportSkillModal", () => ({ ImportSkillModal: () => null }));

import { SkillsTab } from "../experts-panel/skills/SkillsTab";

function organizationSkill(mandatory: boolean) {
  return {
    name: mandatory ? "security-policy" : "weekly-report",
    displayName: mandatory ? "安全规范" : "组织周报",
    description: mandatory ? "执行组织安全规范" : "生成团队周报",
    scope: "server",
    enabled: true,
    path: `/tmp/skills/organization/${mandatory ? "security" : "weekly"}/SKILL.md`,
    version: "1.2.0",
    orgManaged: true,
    orgSkillId: mandatory ? "skill-security" : "skill-weekly",
    orgVersionId: "version-1",
    orgScopeKind: mandatory ? "org" : "team",
    orgMandatory: mandatory,
    orgAllowPersonalOverride: !mandatory,
  };
}

describe("专家技能页的组织 Skill 集成", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.skillsToggle.mockReset().mockResolvedValue(undefined);
    mocks.internalReload.mockReset().mockResolvedValue(undefined);
    localStorage.clear();
    mocks.skillsList.mockResolvedValue([
      organizationSkill(true),
      organizationSkill(false),
      {
        name: "local-analysis",
        displayName: "本地分析",
        description: "本地用户技能",
        scope: "user",
        enabled: true,
        path: "/tmp/skills/local-analysis/SKILL.md",
      },
    ]);
    mocks.orgSetSkillPreference.mockResolvedValue({ skillId: "skill-weekly", enabled: false });
    mocks.listenOrgSkillsChanged.mockResolvedValue(vi.fn());
    mocks.orgSession.mockResolvedValue({
      loggedIn: true,
      bootstrap: { policy: { allowSkillSubmission: true } },
    });
    mocks.orgListScopes.mockResolvedValue([
      { id: "personal", kind: "personal", name: "我的空间" },
      { id: "team-1", kind: "team", name: "研发团队" },
    ]);
    mocks.orgSubmitSkill.mockResolvedValue({
      submissionId: "submission-1",
      skillId: "shared-1",
      version: "1.0.0",
      state: "pending",
    });
  });

  it("技能页直接读取全局目录且不再要求选择来源", async () => {
    render(<SkillsTab pills={<span>页签</span>} />);

    expect(await screen.findByText("安全规范")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "已安装技能" })).toBeInTheDocument();
    expect(screen.queryByText("选择来源目录")).not.toBeInTheDocument();
    expect(mocks.skillsList).toHaveBeenCalled();
    expect(screen.getByText("组织周报")).toBeInTheDocument();
    expect(screen.getByText("全组织共享")).toBeInTheDocument();
    expect(screen.getByText("团队共享")).toBeInTheDocument();
    expect(screen.getAllByText("组织托管")).toHaveLength(2);
    expect(screen.getAllByText("v1.2.0")).toHaveLength(2);
  });

  it("组织强制 Skill 不可停用，可选 Skill 通过服务器偏好同步", async () => {
    render(<SkillsTab pills={<span>页签</span>} />);
    await screen.findByText("安全规范");

    expect(screen.getByRole("checkbox", { name: "卸载安全规范" })).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: "卸载组织周报" }));

    await waitFor(() => expect(mocks.orgSetSkillPreference)
      .toHaveBeenCalledWith("skill-weekly", false));
  });

  it("组织技能变化后的新扫描不会被旧扫描迟到结果覆盖", async () => {
    let resolveOld!: (skills: ReturnType<typeof organizationSkill>[]) => void;
    let notify!: () => void;
    mocks.skillsList.mockReturnValueOnce(new Promise((resolve) => { resolveOld = resolve; })).mockResolvedValueOnce([]);
    mocks.listenOrgSkillsChanged.mockImplementationOnce(async (handler) => { notify = handler; return vi.fn(); });
    render(<SkillsTab pills={null} />);
    await waitFor(() => expect(mocks.listenOrgSkillsChanged).toHaveBeenCalled());
    await act(async () => notify());
    await screen.findByText("还没有已安装的技能");
    await act(async () => resolveOld([organizationSkill(false)]));
    expect(screen.queryByText("组织周报")).toBeNull();
    expect(screen.getByText("还没有已安装的技能")).toBeInTheDocument();
  });

  it("本地 Skill 可直接选择组织范围并打包上传", async () => {
    render(<SkillsTab pills={<span>页签</span>} />);
    fireEvent.click(await screen.findByRole("button", { name: "技能 本地分析更多操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "上传本地分析到组织" }));

    expect(await screen.findByRole("heading", { name: "上传到组织" })).toBeInTheDocument();
    expect(screen.getByRole("combobox")).toHaveValue("team-1");
    fireEvent.click(screen.getByRole("button", { name: "提交分享" }));

    await waitFor(() => expect(mocks.orgSubmitSkill).toHaveBeenCalledWith(
      "/tmp/skills/local-analysis/SKILL.md",
      "team-1",
    ));
  });
});

it("workspace changes keep skill list and toggle on the same cwd, including a late mutation ACK", async () => {
  let finish!: () => void;
  mocks.skillsToggle.mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
  mocks.skillsList.mockImplementation(async (cwd: string) => [{ name: cwd === "/synthetic/A" ? "skill-A" : "skill-B", enabled: true, scope: "local" }]);
  const { rerender } = render(<SkillsTab cwd="/synthetic/A" pills={null} />);
  await screen.findByText("skill-A");
  fireEvent.click(screen.getByRole("checkbox", { name: "停用skill-A" }));
  expect(mocks.skillsToggle).toHaveBeenCalledWith("skill-A", false, "/synthetic/A");
  rerender(<SkillsTab cwd="/synthetic/B" pills={null} />);
  await screen.findByText("skill-B");
  await act(async () => { finish(); });
  expect(mocks.skillsList).toHaveBeenLastCalledWith("/synthetic/B");
  expect(screen.queryByText("skill-A")).not.toBeInTheDocument();
});

it("explicit refresh waits for the runtime ACK before publishing the refreshed skill list", async () => {
  let finish!: () => void;
  mocks.internalReload.mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
  mocks.skillsList.mockResolvedValue([{ name: "refresh-marker", enabled: true }]);
  render(<SkillsTab cwd="/synthetic" pills={null} />);
  await screen.findByText("refresh-marker");
  const refresh = screen.getByRole("button", { name: "刷新" });
  // Rendering the list precedes the async scan's finally block. Wait for the
  // real enabled state before clicking, especially under the full suite load.
  await waitFor(() => expect(refresh).toBeEnabled());
  const before = mocks.skillsList.mock.calls.length;
  fireEvent.click(refresh);
  expect(mocks.internalReload).toHaveBeenCalledWith("skills");
  expect(mocks.skillsList).toHaveBeenCalledTimes(before);
  expect(screen.getByRole("button", { name: "刷新" })).toBeDisabled();
  await act(async () => { finish(); });
  await waitFor(() => expect(mocks.skillsList).toHaveBeenCalledTimes(before + 1));
});
