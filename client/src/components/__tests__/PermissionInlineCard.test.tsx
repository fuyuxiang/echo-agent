import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { resolvePermission, setPermissionMode } = vi.hoisted(() => ({
  resolvePermission: vi.fn(),
  setPermissionMode: vi.fn(),
}));

vi.mock("@/lib/agent-client", () => ({
  agentResolvePermission: resolvePermission,
  permissionModeSet: setPermissionMode,
}));

import { PermissionInlineCard } from "../PermissionDialog";
import { GlobalTooltip } from "../GlobalTooltip";
import { usePermissionStore } from "@/stores/permission-store";
import { usePermissionModeStore } from "@/stores/permission-mode-store";
import { useSessionsStore } from "@/stores/sessions-store";
import type { PermissionRequest } from "@/lib/types";

const request: PermissionRequest = {
  requestId: "permission-1",
  sessionId: "session-1",
  toolCallId: "tool-1",
  toolKind: "execute",
  title: "Execute `python3` with a very long command that must not widen the panel",
  rawInput: { command: "python3 -c 'print(1)'" },
  options: [
    {
      optionId: "enable-always-approve",
      kind: "allow",
      title: "Yes, and don't ask again for anything (always-approve mode)",
    },
    { optionId: "allow-once", kind: "allow", title: "Yes, proceed" },
    { optionId: "reject-once", kind: "deny", title: "No, and tell EchoAgent what to do differently" },
    { optionId: "reject-always-command", kind: "deny_always", title: "Always reject: rm" },
  ],
};

describe("PermissionInlineCard", () => {
  beforeEach(() => {
    resolvePermission.mockReset().mockResolvedValue(true);
    setPermissionMode.mockReset().mockResolvedValue({
      sessionId: "session-1",
      permissionMode: "always-approve",
      configuredPermissionMode: "always-approve",
      autoModeAvailable: true,
      alwaysApproveAvailable: true,
      locked: false,
      runtimeSyncState: "synced",
      runtimeAppliedMode: "always-approve",
      agentRunning: true,
      runtimeSynced: true,
      resolvedPending: 0,
      remainingPending: 1,
      resolvedPermissions: [],
    });
    usePermissionStore.setState({ queues: {}, closedRequestIds: [], priorityRequestBySession: {} });
    usePermissionModeStore.setState({ homeMode: "ask", statuses: {}, capabilityStatus: null });
    useSessionsStore.setState({
      independent: [{
        sessionId: "session-1",
        title: "Task",
        cwd: "/workspace",
        status: "awaiting_permission",
        permissionMode: "ask",
      }],
      pendingSessionPatches: {},
    });
    usePermissionStore.getState().request(request);
  });

  const openOptions = () => fireEvent.click(screen.getByRole("button", { name: "更多授权选项" }));
  const chooseTaskAlways = () => {
    openOptions();
    fireEvent.click(screen.getByRole("menuitem", { name: /^本任务免审批/ }));
  };

  it("默认只突出单次批准和拒绝，长期授权留在下拉中", () => {
    render(<PermissionInlineCard sessionId="session-1" />);
    expect(screen.getByText("是否允许运行这条命令？")).toBeInTheDocument();
    const choices = screen.getByRole("group", { name: "授权选择" });
    expect(within(choices).getAllByRole("button")).toHaveLength(3);
    expect(within(choices).getByRole("button", { name: "允许一次" })).toBeInTheDocument();
    expect(within(choices).getByRole("button", { name: "拒绝本次" })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem")).toBeNull();
    expect(screen.getByText("操作详情").closest("details")).not.toHaveAttribute("open");
    expect(screen.getByLabelText("操作内容")).toHaveTextContent("python3 -c 'print(1)'");
  });

  it("长命令只有一份，可完整展开和收起，新请求从折叠状态开始", () => {
    const scrollHeight = vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(240);
    try {
      const command = '$f = "C:\\Users\\Taurus\\.echo-agent\\sessions\\' + "encoded-cache/".repeat(50) + 'output.txt"; Get-Content $f';
      usePermissionStore.setState({ queues: { "session-1": [{ ...request, title: `Execute \`${command}\``, rawInput: { command } }] } });
      render(<PermissionInlineCard sessionId="session-1" />);
      expect(screen.getByText("是否允许运行这条命令？")).toBeVisible();
      expect(screen.getByLabelText("操作内容").textContent).toBe(command);
      expect(screen.queryByText("操作详情")).toBeNull();
      const toggle = screen.getByRole("button", { name: "展开完整命令" });
      fireEvent.click(toggle);
      expect(toggle).toHaveAttribute("aria-expanded", "true");
      expect(screen.getByLabelText("操作内容")).toHaveClass("is-expanded");
      fireEvent.click(toggle);
      expect(toggle).toHaveAttribute("aria-expanded", "false");
      fireEvent.click(toggle);
      act(() => usePermissionStore.setState({ queues: { "session-1": [{ ...request, requestId: "next-command", title: `Execute \`${command}\``, rawInput: { command } }] } }));
      expect(screen.getByRole("button", { name: "展开完整命令" })).toHaveAttribute("aria-expanded", "false");
    } finally { scrollHeight.mockRestore(); }
  });

  it("有说明才展示一句操作说明，其他参数中不重复命令和说明", () => {
    usePermissionStore.setState({ queues: { "session-1": [{ ...request, title: "Execute `python3 check.py`", rawInput: {
      command: "python3 check.py", description: "核对交付文件，继续整理检查结果。", cwd: "/workspace",
    } }] } });
    const { container } = render(<PermissionInlineCard sessionId="session-1" />);
    expect(screen.getByText("操作说明：核对交付文件，继续整理检查结果。")).toBeVisible();
    fireEvent.click(screen.getByText("操作详情"));
    expect(container.querySelector(".perm-inline__raw")).toHaveTextContent("/workspace");
    expect(container.querySelector(".perm-inline__raw")).not.toHaveTextContent(/python3|核对交付文件/);
  });

  it("授权菜单聚焦和悬停后都不再显示后端英文悬浮提示", async () => {
    render(<><PermissionInlineCard sessionId="session-1" /><GlobalTooltip /></>);
    openOptions();
    const menu = screen.getByRole("menu", { name: "更多授权选项" });
    const taskMode = within(menu).getByRole("menuitem", { name: /^本任务免审批/ });
    await waitFor(() => expect(taskMode).toHaveFocus());
    fireEvent.pointerOver(taskMode);
    // Wait beyond GlobalTooltip's pointer dwell; the menu must stay readable.
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 400)); });
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(menu.querySelector("[title], [data-tip], [data-global-tooltip-native]")).toBeNull();
    expect(menu).not.toHaveTextContent("always-approve mode");
    expect(menu).not.toHaveTextContent("Always reject");
    expect(within(menu).getByText("范围：rm")).toBeInTheDocument();
    expect(setPermissionMode).not.toHaveBeenCalled();
    expect(resolvePermission).not.toHaveBeenCalled();
  });

  it("没有范围的持久授权也用中文说明，提交时保留原始选项", async () => {
    usePermissionStore.setState({ queues: { "session-1": [{ ...request, options: [
      ...request.options,
      { optionId: "runtime-remember", kind: "allow_always", title: "Always allow" },
    ] }] } });
    render(<PermissionInlineCard sessionId="session-1" />);
    openOptions();
    fireEvent.click(screen.getByRole("menuitem", { name: /^始终允许此类操作\s*后续同类请求自动允许$/ }));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "runtime-remember", cancelled: false }));
    expect(setPermissionMode).not.toHaveBeenCalled();
  });

  it("点击授权通知后将对应请求置顶并聚焦", async () => {
    usePermissionStore.getState().request({ ...request, requestId: "permission-2", toolCallId: "tool-2" });
    render(<PermissionInlineCard sessionId="session-1" />);
    expect(screen.getByText("还有 1 项待审批")).toBeInTheDocument();
    act(() => usePermissionStore.getState().promote("permission-2", "session-1"));
    const card = screen.getByRole("region", { name: "操作授权" });
    expect(card).toHaveAttribute("id", "permission-permission-2");
    await waitFor(() => expect(card).toHaveFocus());
    expect(usePermissionStore.getState().priorityRequestBySession["session-1"]).toBeUndefined();
  });

  it("按原 optionId 提交单次授权，在后端确认后关闭卡片", async () => {
    render(<PermissionInlineCard sessionId="session-1" />);
    fireEvent.click(screen.getByRole("button", { name: "允许一次" }));
    expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "allow-once", cancelled: false });
    expect(setPermissionMode).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("region", { name: "操作授权" })).toBeNull());
  });

  it("拒绝只提交当前请求的拒绝选项", async () => {
    render(<PermissionInlineCard sessionId="session-1" />);
    fireEvent.click(screen.getByRole("button", { name: "拒绝本次" }));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "reject-once", cancelled: false }));
    expect(setPermissionMode).not.toHaveBeenCalled();
  });

  it("缺少拒绝选项时取消请求，不会猜测其他选项", async () => {
    usePermissionStore.setState({ queues: { "session-1": [{ ...request, options: request.options.filter(o => o.kind !== "deny") }] } });
    render(<PermissionInlineCard sessionId="session-1" />);
    fireEvent.click(screen.getByRole("button", { name: "拒绝本次" }));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: undefined, cancelled: true }));
  });

  it("本任务免审批先明确确认，再同步模式并批准当前操作", async () => {
    render(<PermissionInlineCard sessionId="session-1" />);
    chooseTaskAlways();
    const dialog = screen.getByRole("alertdialog", { name: "确认本任务免审批" });
    expect(within(dialog).getByText(/网页与电脑操作仍遵守各自的授权规则/)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "取消" })).toHaveFocus();
    expect(setPermissionMode).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "确认并允许" }));
    await waitFor(() => expect(setPermissionMode).toHaveBeenCalledWith("session-1", "always-approve"));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "enable-always-approve", cancelled: false }));
    expect(setPermissionMode.mock.invocationCallOrder[0]).toBeLessThan(resolvePermission.mock.invocationCallOrder[0]);
    expect(usePermissionModeStore.getState().statuses["session-1"]?.permissionMode).toBe("always-approve");
    expect(useSessionsStore.getState().independent[0].permissionMode).toBe("always-approve");
    await waitFor(() => expect(screen.queryByRole("region", { name: "操作授权" })).toBeNull());
  });

  it("模式切换失败时保留请求且不会批准当前操作", async () => {
    setPermissionMode.mockRejectedValue(new Error("权限模式已被组织策略锁定"));
    render(<PermissionInlineCard sessionId="session-1" />);
    chooseTaskAlways();
    fireEvent.click(screen.getByRole("button", { name: "确认并允许" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("权限模式已被组织策略锁定");
    expect(resolvePermission).not.toHaveBeenCalled();
    expect(screen.getByRole("region", { name: "操作授权" })).toBeInTheDocument();
  });

  it("精确范围授权保留原 optionId，不触发整个任务的权限切换", async () => {
    usePermissionStore.setState({ queues: { "session-1": [{ ...request, options: [...request.options,
      { optionId: "allow-always-command", kind: "allow_always", title: "Always allow: python3" },
    ] }] } });
    render(<PermissionInlineCard sessionId="session-1" />);
    openOptions();
    fireEvent.click(screen.getByRole("menuitem", { name: /^始终允许此类操作\s*范围：python3$/ }));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "allow-always-command", cancelled: false }));
    expect(setPermissionMode).not.toHaveBeenCalled();
  });

  it.each([
    ["deny_always", "Never allow: git", "范围：git"],
    ["allow_always", "Yes, always allow example.com for this project", "项目范围：example.com（跨任务生效）"],
    ["deny_always", "No, never allow www.example.com for this project", "项目范围：www.example.com（跨任务生效）"],
  ] as const)("持续授权显示 Runtime 实际给出的范围：%s / %s", async (kind, title, scope) => {
    usePermissionStore.setState({ queues: { "session-1": [{ ...request, options: [
      ...request.options, { optionId: "runtime-scoped", kind, title },
    ] }] } });
    render(<PermissionInlineCard sessionId="session-1" />);
    openOptions();
    fireEvent.click(screen.getByText(scope));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "runtime-scoped", cancelled: false }));
    expect(setPermissionMode).not.toHaveBeenCalled();
  });

  it("提权已成功但当前批准失败时明确区分两种结果并保留请求", async () => {
    resolvePermission.mockResolvedValue(false);
    render(<PermissionInlineCard sessionId="session-1" />);
    chooseTaskAlways();
    fireEvent.click(screen.getByRole("button", { name: "确认并允许" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("已开启本任务免审批，但当前操作尚未批准");
    expect(usePermissionModeStore.getState().statuses["session-1"].permissionMode).toBe("always-approve");
    expect(usePermissionStore.getState().queues["session-1"]).toHaveLength(1);
    expect(screen.getByRole("button", { name: "允许一次" })).toBeEnabled();
  });

  it("未知选项可从菜单操作，无单次批准选项时不伪造允许按钮", async () => {
    usePermissionStore.setState({ queues: { "session-1": [{ ...request, options: [{ optionId: "runtime-choice", kind: "other", title: "Runtime choice" }] }] } });
    render(<PermissionInlineCard sessionId="session-1" />);
    expect(screen.queryByRole("button", { name: "允许一次" })).toBeNull();
    openOptions();
    fireEvent.click(screen.getByRole("menuitem", { name: "Runtime choice" }));
    await waitFor(() => expect(resolvePermission).toHaveBeenCalledWith("permission-1", { optionId: "runtime-choice", cancelled: false }));
  });

  it("菜单 Escape 和取消提权都只关闭浮层并返回焦点，不提交授权", async () => {
    render(<PermissionInlineCard sessionId="session-1" />);
    openOptions();
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    expect(screen.getByRole("button", { name: "更多授权选项" })).toHaveFocus();
    chooseTaskAlways();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "更多授权选项" })).toHaveFocus());
    expect(resolvePermission).not.toHaveBeenCalled();
    expect(setPermissionMode).not.toHaveBeenCalled();
  });

  it("提交期间防止重复授权，失败时保留请求以便重试", async () => {
    let finish!: (value: boolean) => void;
    resolvePermission.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
    render(<PermissionInlineCard sessionId="session-1" />);
    const allow = screen.getByRole("button", { name: "允许一次" });
    fireEvent.click(allow); fireEvent.click(allow);
    expect(resolvePermission).toHaveBeenCalledTimes(1);
    await act(async () => finish(false));
    expect(screen.getByRole("alert")).toHaveTextContent("该请求已失效");
    expect(screen.getByRole("button", { name: "允许一次" })).not.toBeDisabled();
    expect(usePermissionStore.getState().queues["session-1"]).toHaveLength(1);
  });

  it("切换任务后旧请求的迟到失败不会污染新任务", async () => {
    let fail!: (reason: Error) => void;
    resolvePermission.mockImplementation(() => new Promise((_resolve, reject) => { fail = reject; }));
    usePermissionStore.getState().request({ ...request, requestId: "other", sessionId: "session-2" });
    const view = render(<PermissionInlineCard sessionId="session-1" />);
    fireEvent.click(screen.getByRole("button", { name: "允许一次" }));
    view.rerender(<PermissionInlineCard sessionId="session-2" />);
    await act(async () => fail(new Error("旧请求失败")));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("region", { name: "操作授权" })).toHaveAttribute("id", "permission-other");
    expect(usePermissionStore.getState().queues["session-2"]).toHaveLength(1);
  });
});
