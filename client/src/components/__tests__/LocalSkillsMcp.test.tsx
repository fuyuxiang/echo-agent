import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

const mocks = vi.hoisted(() => ({
  inspect: vi.fn(),
  inspectMany: vi.fn(),
  install: vi.fn(),
  catalogRead: vi.fn(),
  mcpList: vi.fn(),
  mcpSetup: vi.fn(),
  mcpToggleTool: vi.fn(),
  pickFiles: vi.fn(),
  pickDirectory: vi.fn(),
  connectorConfig: vi.fn(),
  openUrl: vi.fn(),
}));

vi.mock("@/stores/session-store", () => {
  const state = { sessionId: "session-1" };
  const useSessionStore = Object.assign(
    (selector: (value: typeof state) => unknown) => selector(state),
    { getState: () => state },
  );
  return { useSessionStore };
});
vi.mock("@/lib/ensure-session", () => ({ ensureSession: vi.fn(async () => "session-1") }));
vi.mock("@/lib/agent-client", () => ({
  skillsInspectPackage: mocks.inspect,
  skillsInspectPackages: mocks.inspectMany,
  skillsInstallPackage: mocks.install,
  skillsCatalogReadSkill: mocks.catalogRead,
  filesystemPickFiles: mocks.pickFiles,
  filesystemPickDirectory: mocks.pickDirectory,
  mcpList: mocks.mcpList,
  mcpSetup: mocks.mcpSetup,
  mcpToggleTool: mocks.mcpToggleTool,
  mcpDelete: vi.fn(),
  mcpToggle: vi.fn(),
  onMcpStatusEvent: vi.fn(async () => () => {}),
  connectorsReadMcpConfig: mocks.connectorConfig,
  openUrl: mocks.openUrl,
  mcpConfigPath: vi.fn(),
  mcpConfigRead: vi.fn(),
  mcpConfigSave: vi.fn(),
}));

import { ImportSkillModal } from "../experts-panel/skills/ImportSkillModal";
import { SkillDetailModal } from "../experts-panel/skills/SkillDetailModal";
import { McpModal } from "../experts-panel/connectors/McpModal";
import { ConnectorsTab } from "../experts-panel/connectors/ConnectorsTab";
import { ConnectorDetailModal } from "../experts-panel/connectors/ConnectorDetailModal";
import { ConnectorTokenForm } from "../experts-panel/connectors/ConnectorTokenForm";

describe("本地 Skills / MCP 完整流程", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.pickFiles.mockResolvedValue(["/tmp/demo-skill"]);
    mocks.pickDirectory.mockResolvedValue("/tmp/demo-skill");
    const inspection = {
      sourcePath: "/tmp/demo-skill",
      name: "demo-skill",
      description: "Demo",
      version: "1.0.0",
      fileCount: 2,
      totalBytes: 100,
      riskLevel: "medium",
      findings: [{
        level: "medium",
        code: "executable-script",
        message: "包含可执行脚本，安装前应审查源码",
        path: "scripts/run.sh",
      }],
      warnings: [],
      capability: {
        declared: true,
        state: "configuration_required",
        ready: false,
        capabilities: ["notion.page.create"],
        checks: [{
          kind: "connector",
          key: "notion",
          status: "configuration_required",
          message: "Connect an account for Notion",
        }],
        manifest: {
          schemaVersion: 1,
          capabilities: ["notion.page.create"],
          requirements: {
            commands: [],
            connectors: [{ id: "notion", label: "Notion", accountRequired: true }],
            osPermissions: [],
          },
          permissions: { filesystem: "none", network: [], externalActions: [] },
          artifacts: [],
        },
      },
      sourceHash: "a".repeat(64),
      alreadyInstalled: false,
    };
    mocks.inspect.mockResolvedValue(inspection);
    mocks.inspectMany.mockResolvedValue([{ label: "demo-skill", inspection }]);
    mocks.install.mockResolvedValue({
      installedPath: "/tmp/managed/demo-skill",
      updated: false,
      inspection: { name: "demo-skill" },
    });
    mocks.catalogRead.mockResolvedValue("# Demo skill\n\nUse it carefully.");
    mocks.connectorConfig.mockResolvedValue('{"mcpServers":{}}');
    mocks.openUrl.mockResolvedValue(undefined);
    mocks.mcpList.mockResolvedValue([]);
  });

  it("中风险 Skill 先展示报告，不自动安装，确认后才提交", async () => {
    render(<ImportSkillModal onClose={vi.fn()} />);
    fireEvent.click(screen.getByText(/点击选择 Markdown/).closest(".sk-drop")!);

    expect(await screen.findByText("中风险")).toBeInTheDocument();
    expect(screen.getByText("包含可执行脚本，安装前应审查源码")).toBeInTheDocument();
    expect(screen.getByText("待连接/授权")).toBeInTheDocument();
    expect(screen.getByText("待连接账号：Notion")).toBeInTheDocument();
    expect(mocks.install).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "安装技能" }));
    await waitFor(() => expect(mocks.install).toHaveBeenCalledWith(
      "/tmp/demo-skill",
      "a".repeat(64),
      false,
    ));
  });

  it("低风险 Skill 默认也只检查，需要用户显式点击安装", async () => {
    const inspection = {
      sourcePath: "/tmp/demo-skill",
      name: "demo-skill",
      description: "Demo",
      version: "1.0.0",
      fileCount: 1,
      totalBytes: 32,
      riskLevel: "low",
      findings: [],
      warnings: [],
      sourceHash: "b".repeat(64),
      alreadyInstalled: false,
    };
    mocks.inspectMany.mockResolvedValueOnce([{ label: "demo-skill", inspection }]);
    render(<ImportSkillModal onClose={vi.fn()} />);

    expect(screen.getByRole("checkbox", { name: /低风险时自动安装/ })).not.toBeChecked();
    fireEvent.click(screen.getByText(/点击选择 Markdown/).closest(".sk-drop")!);

    expect(await screen.findByText("低风险")).toBeInTheDocument();
    expect(mocks.install).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "安装技能" }));
    await waitFor(() => expect(mocks.install).toHaveBeenCalledWith(
      "/tmp/demo-skill",
      "b".repeat(64),
      false,
    ));
  });

  it("已由后端目录加载的 Skill 详情使用原始 sourceDir 检查并安装", async () => {
    mocks.inspect.mockResolvedValueOnce({
      sourcePath: "/trusted/catalog/demo",
      name: "demo",
      description: "Demo",
      fileCount: 1,
      totalBytes: 32,
      riskLevel: "low",
      findings: [],
      warnings: [],
      sourceHash: "c".repeat(64),
      alreadyInstalled: false,
    });
    const skill = {
      id: "demo",
      name: "Demo",
      desc: "Demo skill",
      sourceDir: "/trusted/catalog/demo",
      origin: "builtin" as const,
      cat: "other",
      featured: true,
    };
    render(<SkillDetailModal skill={skill} onClose={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "安装技能" }));

    await waitFor(() => expect(mocks.inspect).toHaveBeenCalledWith("/trusted/catalog/demo"));
    await waitFor(() => expect(mocks.install).toHaveBeenCalledWith(
      "/trusted/catalog/demo",
      "c".repeat(64),
      false,
    ));
  });

  it("低风险技能需要外部账号时，先告知用户再安装", async () => {
    mocks.inspect.mockResolvedValueOnce({
      sourcePath: "/trusted/catalog/notion-writer",
      name: "notion-writer",
      description: "Write to Notion",
      fileCount: 1,
      totalBytes: 32,
      riskLevel: "low",
      findings: [],
      warnings: [],
      capability: {
        declared: true,
        state: "configuration_required",
        ready: false,
        capabilities: ["notion.page.create"],
        checks: [{
          kind: "connector",
          key: "notion",
          status: "configuration_required",
          message: "Connect an account for Notion",
        }],
        manifest: {
          schemaVersion: 1,
          capabilities: ["notion.page.create"],
          requirements: {
            commands: [],
            connectors: [{ id: "notion", label: "Notion", accountRequired: true }],
            osPermissions: [],
          },
          permissions: { filesystem: "none", network: [], externalActions: [] },
          artifacts: [],
        },
      },
      sourceHash: "e".repeat(64),
      alreadyInstalled: false,
    });
    const skill = {
      id: "notion-writer",
      name: "Notion Writer",
      desc: "Write to Notion",
      sourceDir: "/trusted/catalog/notion-writer",
      origin: "builtin" as const,
      cat: "other",
      featured: false,
    };
    render(<SkillDetailModal skill={skill} onClose={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "安装技能" }));

    const dialog = await screen.findByRole("alertdialog", { name: "确认安装需审查技能" });
    expect(within(dialog).getByText("待连接/授权")).toBeInTheDocument();
    expect(within(dialog).getByText("待连接账号：Notion")).toBeInTheDocument();
    expect(mocks.install).not.toHaveBeenCalled();

    fireEvent.click(within(dialog).getByRole("button", { name: "确认安装" }));
    await waitFor(() => expect(mocks.install).toHaveBeenCalledWith(
      "/trusted/catalog/notion-writer",
      "e".repeat(64),
      false,
    ));
  });

  it("Skill 详情在后端拒绝未授权 sourceDir 时不会继续安装", async () => {
    mocks.inspect.mockRejectedValueOnce(new Error("技能包来源未授权"));
    const onToast = vi.fn();
    const skill = {
      id: "forged",
      name: "Forged",
      desc: "forged path",
      sourceDir: "/private/secret",
      origin: "builtin" as const,
      cat: "other",
      featured: false,
    };
    render(<SkillDetailModal skill={skill} onClose={vi.fn()} onToast={onToast} />);
    fireEvent.click(await screen.findByRole("button", { name: "安装技能" }));

    await waitFor(() => expect(onToast).toHaveBeenCalledWith(expect.stringContaining("未授权")));
    expect(mocks.install).not.toHaveBeenCalled();
  });

  it("Skill 预览失败时显示重试且禁止安装", async () => {
    mocks.catalogRead
      .mockReset()
      .mockRejectedValueOnce(new Error("源码不可读"))
      .mockResolvedValueOnce("# Recovered");
    const skill = {
      id: "preview-failure",
      name: "Preview Failure",
      desc: "preview",
      sourceDir: "/trusted/catalog/preview-failure",
      origin: "builtin" as const,
      cat: "other",
      featured: false,
    };
    render(<SkillDetailModal skill={skill} onClose={vi.fn()} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("技能预览加载失败：源码不可读");
    expect(screen.getByRole("button", { name: "安装技能" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "重试加载预览" }));
    expect(await screen.findByText("Recovered")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "安装技能" })).toBeEnabled();
  });

  it("高风险 Skill 使用应用内确认，Escape 只关闭内层并恢复焦点", async () => {
    const riskInspection = {
      sourcePath: "/trusted/catalog/risky",
      name: "risky",
      description: "Risky",
      fileCount: 1,
      totalBytes: 32,
      riskLevel: "high",
      findings: [{ level: "high", code: "shell", message: "包含高风险脚本", path: "run.sh" }],
      warnings: [],
      sourceHash: "d".repeat(64),
      alreadyInstalled: false,
    };
    mocks.inspect.mockResolvedValue(riskInspection);
    const skill = {
      id: "risky",
      name: "Risky",
      desc: "Risky skill",
      sourceDir: "/trusted/catalog/risky",
      origin: "builtin" as const,
      cat: "other",
      featured: false,
    };
    render(<SkillDetailModal skill={skill} onClose={vi.fn()} />);
    const install = await screen.findByRole("button", { name: "安装技能" });
    install.focus();
    fireEvent.click(install);

    const riskDialog = await screen.findByRole("alertdialog", { name: "确认安装高风险技能" });
    expect(within(riskDialog).getByRole("button", { name: "取消" })).toHaveFocus();
    expect(mocks.install).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog", { name: "确认安装高风险技能" })).toBeNull();
    expect(screen.getByRole("dialog", { name: "Risky 技能详情" })).toBeInTheDocument();
    await waitFor(() => expect(install).toHaveFocus());

    fireEvent.click(install);
    fireEvent.click(await screen.findByRole("button", { name: "仍要安装" }));
    await waitFor(() => expect(mocks.install).toHaveBeenCalledWith(
      "/trusted/catalog/risky",
      "d".repeat(64),
      true,
    ));
  });

  it("连接器配置预览失败时显示错误并可重试", async () => {
    mocks.connectorConfig
      .mockReset()
      .mockRejectedValueOnce(new Error("配置文件损坏"))
      .mockResolvedValueOnce('{"mcpServers":{"demo":{}}}');
    render(
      <ConnectorDetailModal
        connector={{
          id: "demo",
          name: "Demo",
          desc: "Demo connector",
          source: "demo",
          kind: "mcp",
          examplesZh: [],
          cat: "other",
        }}
        root="/trusted/catalog"
        onClose={vi.fn()}
        onConfigure={vi.fn()}
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent("MCP 配置预览加载失败：配置文件损坏");
    fireEvent.click(screen.getByRole("button", { name: "重试加载配置" }));
    expect(await screen.findByText(/mcpServers/)).toBeInTheDocument();
  });

  it("Token 提交防重复并保留失败输入，未保存关闭可继续编辑", async () => {
    let reject!: (reason: Error) => void;
    const submit = vi.fn().mockImplementationOnce(() => new Promise<void>((_, fail) => { reject = fail; }));
    const close = vi.fn();
    render(<ConnectorTokenForm connector={{
      id: "token-focus", name: "Token Focus", desc: "Demo", source: "test", kind: "mcp", examplesZh: [], cat: "other",
      tokenSchema: { fields: [{ key: "TOKEN", label: "Token", required: true }] },
    }} onClose={close} onSubmit={submit} />);
    const form = screen.getByRole("dialog", { name: "Token Focus 授权" });
    const token = within(form).getByRole("textbox");
    fireEvent.change(token, { target: { value: "test-token" } });
    fireEvent.submit(form);
    fireEvent.submit(form);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(submit).toHaveBeenCalledTimes(1);
    expect(close).not.toHaveBeenCalled();
    expect(within(form).getByRole("button", { name: "关闭" })).toBeDisabled();
    await act(async () => { reject(new Error("服务暂不可用")); });
    expect(await screen.findByRole("alert")).toHaveTextContent("连接失败：服务暂不可用");
    expect(token).toHaveValue("test-token");
    fireEvent.click(within(form).getByRole("button", { name: "关闭" }));
    expect(screen.getByRole("alertdialog", { name: "舍弃未保存的修改？" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" }));
    expect(token).toHaveValue("test-token");
    expect(close).not.toHaveBeenCalled();
  });

  it("Token 帮助链接通过受控后端打开，失败时在弹窗内提示", async () => {
    mocks.openUrl.mockRejectedValueOnce(new Error("不允许的 URL scheme"));
    render(
      <ConnectorTokenForm
        connector={{
          id: "token-demo",
          name: "Token Demo",
          desc: "Demo",
          source: "token-demo",
          kind: "mcp",
          examplesZh: [],
          cat: "other",
          tokenSchema: {
            docUrl: "javascript:alert(1)",
            docLabel: "查看帮助",
            fields: [{ key: "TOKEN", label: "Token", required: true }],
          },
        }}
        onClose={vi.fn()}
        onSubmit={vi.fn()}
      />,
    );

    expect(screen.queryByRole("link", { name: "查看帮助" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "查看帮助" }));
    await waitFor(() => expect(mocks.openUrl).toHaveBeenCalledWith("javascript:alert(1)"));
    expect(await screen.findByRole("alert")).toHaveTextContent("无法打开帮助链接：不允许的 URL scheme");
  });

  it("MCP Setup 与逐工具开关走活动会话", async () => {
    mocks.mcpList.mockResolvedValue([{
      name: "demo",
      displayName: "Demo MCP",
      transport: "stdio",
      target: "demo-mcp",
      enabled: true,
      source: "local",
      status: "setuprequired",
      live: true,
      authRequired: false,
      setupRequired: true,
      setup: {
        fields: [{
          id: "region",
          label: "Region",
          type: "select",
          required: true,
          options: [{ label: "US", value: "us" }],
        }],
        variables: {},
      },
      setupValues: {},
      tools: [{ name: "read_file", displayName: "Read file", enabled: true }],
      args: [],
      env: {},
      editable: true,
    }]);
    render(<McpModal onClose={vi.fn()} />);

    fireEvent.click(await screen.findByText("Demo MCP"));
    fireEvent.change(screen.getByLabelText(/Region/), { target: { value: "us" } });
    fireEvent.click(screen.getByRole("button", { name: "应用并启动" }));
    await waitFor(() => expect(mocks.mcpSetup).toHaveBeenCalledWith(
      "session-1", "demo", { region: "us" },
    ));

    const tool = screen.getByText("Read file").closest<HTMLElement>(".mcp-tool")!;
    fireEvent.click(within(tool).getByRole("checkbox"));
    await waitFor(() => expect(mocks.mcpToggleTool).toHaveBeenCalledWith(
      "session-1", "demo", "read_file", false,
    ));
  });

  it("连接器页直接展示全局 MCP 管理，不再要求额外跳转", async () => {
    render(<ConnectorsTab pills={<span>连接器页签</span>} />);

    expect(await screen.findByText("暂无 MCP 服务")).toBeInTheDocument();
    expect(screen.getByText("MCP 服务管理")).toBeInTheDocument();
    expect(screen.queryByText("查看全局连接器")).not.toBeInTheDocument();
    expect(document.querySelector(".modal-overlay")).not.toBeInTheDocument();
  });
});
