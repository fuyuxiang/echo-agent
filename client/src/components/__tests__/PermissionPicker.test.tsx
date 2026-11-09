import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const mocks = vi.hoisted(() => ({
  permissionModeGet: vi.fn(),
  permissionModeSet: vi.fn(),
  groupPermissionModeGet: vi.fn(),
  groupPermissionModeSet: vi.fn(),
}));
vi.mock("@/features/groups/group-client", () => ({
  groupPermissionModeGet: (...args: unknown[]) => mocks.groupPermissionModeGet(...args),
  groupPermissionModeSet: (...args: unknown[]) => mocks.groupPermissionModeSet(...args),
}));

vi.mock("@/lib/agent-client", () => ({
  permissionModeGet: (...args: unknown[]) => mocks.permissionModeGet(...args),
  permissionModeSet: (...args: unknown[]) => mocks.permissionModeSet(...args),
}));

import { PermissionPicker } from "../PermissionPicker";
import type {
  PermissionMode,
  PermissionModeSetResult,
  PermissionModeStatus,
} from "@/lib/agent-client";
import type { PermissionRequest, SessionSummary } from "@/lib/types";
import { selectProjectPermissionMode, usePermissionModeStore } from "@/stores/permission-mode-store";
import { usePermissionStore } from "@/stores/permission-store";
import { useSessionsStore } from "@/stores/sessions-store";

function modeStatus(
  sessionId?: string,
  overrides: Partial<PermissionModeStatus> = {},
): PermissionModeStatus {
  return {
    sessionId,
    permissionMode: "ask",
    configuredPermissionMode: "ask",
    autoModeAvailable: true,
    alwaysApproveAvailable: true,
    locked: false,
    runtimeSyncState: sessionId ? "synced" : "offline",
    runtimeAppliedMode: sessionId ? "ask" : undefined,
    ...overrides,
  };
}

function setResult(
  sessionId: string,
  mode: PermissionMode,
  overrides: Partial<PermissionModeSetResult> = {},
): PermissionModeSetResult {
  return {
    ...modeStatus(sessionId, {
      permissionMode: mode,
      configuredPermissionMode: mode,
      runtimeSyncState: "synced",
      runtimeAppliedMode: mode,
    }),
    agentRunning: true,
    runtimeSynced: true,
    resolvedPending: 0,
    remainingPending: 0,
    resolvedPermissions: [],
    ...overrides,
  };
}

function session(sessionId: string, permissionMode: PermissionMode = "ask"): SessionSummary {
  return {
    sessionId,
    title: sessionId,
    cwd: "/workspace",
    status: "pending",
    permissionMode,
  };
}

function permission(requestId: string, sessionId: string): PermissionRequest {
  return {
    requestId,
    sessionId,
    toolCallId: `tool-${requestId}`,
    toolKind: "edit",
    title: "Write test file",
    options: [{ optionId: "allow", kind: "allow", title: "Yes" }],
  };
}

describe("PermissionPicker", () => {
  beforeEach(() => {
    mocks.permissionModeGet.mockReset().mockResolvedValue(modeStatus());
    mocks.permissionModeSet.mockReset();
    mocks.groupPermissionModeGet.mockReset().mockResolvedValue(modeStatus());
    mocks.groupPermissionModeSet.mockReset().mockImplementation(async (_id, mode) => modeStatus(undefined, {permissionMode:mode, configuredPermissionMode:mode, runtimeAppliedMode:mode}));
    usePermissionModeStore.setState({
      homeMode: "ask",
      statuses: {},
      capabilityStatus: null,
      projectPermissionRead: null,
    });
    usePermissionStore.setState({ queues: {}, closedRequestIds: [] });
    useSessionsStore.setState({ independent: [], pendingSessionPatches: {} });
  });

  it("专家团权限只写专家团，离线也可保存且不会修改首页或单个任务", async () => {
    usePermissionModeStore.setState({homeMode:"always-approve"});
    mocks.groupPermissionModeGet.mockResolvedValue(modeStatus(undefined, {runtimeSyncState:"offline"}));
    const user = userEvent.setup();
    render(<PermissionPicker groupId="group-a" groupPermissionMode="ask" />);
    await waitFor(() => expect(mocks.groupPermissionModeGet).toHaveBeenCalledWith("group-a"));
    await user.click(screen.getByRole("button", {name:"专家团权限：请求批准"}));
    expect(screen.getByRole("menu", {name:"本专家团权限"})).toHaveTextContent("只读讨论始终保持只读");
    await user.click(screen.getByRole("menuitemradio", {name:/^自动判断/}));
    expect(mocks.groupPermissionModeSet).toHaveBeenCalledWith("group-a","auto");
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
    expect(mocks.permissionModeGet).not.toHaveBeenCalled();
    expect(usePermissionModeStore.getState().homeMode).toBe("always-approve");
    expect(usePermissionModeStore.getState().capabilityStatus).toBeNull();
    expect(screen.getByRole("button", {name:"专家团权限：自动判断"})).toBeInTheDocument();
  });

  it("专家团免审批明确确认全部成员及长期范围，切换失败保留原模式并可重试", async () => {
    mocks.groupPermissionModeSet.mockRejectedValueOnce(new Error("成员同步失败"));
    const user = userEvent.setup();
    render(<PermissionPicker groupId="group-a" />);
    await user.click(screen.getByRole("button",{name:"专家团权限：请求批准"}));
    await user.click(screen.getByRole("menuitemradio",{name:/^本专家团免审批/}));
    const confirm=screen.getByRole("alertdialog",{name:"确认本专家团免审批"});
    expect(confirm).toHaveTextContent("协调员与全部成员");
    expect(confirm).toHaveTextContent("设置会保留到你再次修改");
    expect(mocks.groupPermissionModeSet).not.toHaveBeenCalled();
    await user.click(within(confirm).getByRole("button",{name:"确认开启"}));
    expect(await screen.findByRole("alert")).toHaveTextContent("成员同步失败");
    expect(screen.getByRole("button",{name:"专家团权限：请求批准"})).toBeInTheDocument();
    await user.click(screen.getByRole("button",{name:"确认开启"}));
    expect(await screen.findByRole("button",{name:"专家团权限：本专家团免审批"})).toBeInTheDocument();
  });

  it("专家团读取失败时禁止猜测权限并提供重试，组织锁定后禁用选择", async () => {
    mocks.groupPermissionModeGet.mockRejectedValueOnce(new Error("暂时不可用"))
      .mockResolvedValueOnce(modeStatus(undefined,{locked:true,lockedReason:"组织锁定为请求批准"}));
    const user=userEvent.setup();
    render(<PermissionPicker groupId="group-a" />);
    await user.click(screen.getByRole("button",{name:"专家团权限：请求批准"}));
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时不可用");
    expect(screen.getByRole("menuitemradio",{name:/^自动判断/})).toBeDisabled();
    await user.click(screen.getByRole("button",{name:"重试读取权限"}));
    await screen.findByText("组织锁定为请求批准");
    expect(screen.getAllByRole("menuitemradio").every(item=>(item as HTMLButtonElement).disabled)).toBe(true);
  });

  it("快速切换专家团时丢弃旧读写响应", async () => {
    let finish!: (value: PermissionModeStatus) => void;
    mocks.groupPermissionModeSet.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));
    const user=userEvent.setup();
    const view=render(<PermissionPicker groupId="group-a" />);
    await user.click(screen.getByRole("button",{name:"专家团权限：请求批准"}));
    await user.click(screen.getByRole("menuitemradio",{name:/^自动判断/}));
    view.rerender(<PermissionPicker groupId="group-b" />);
    await act(async()=>{finish(modeStatus(undefined,{permissionMode:"auto",configuredPermissionMode:"auto"}));});
    expect(screen.getByRole("button",{name:"专家团权限：请求批准"})).toBeInTheDocument();
    expect(mocks.groupPermissionModeGet).toHaveBeenCalledWith("group-b");
  });

  it("离开专家团后，迟到的保存结果不会弹出其他专家团的反馈或修改首页", async () => {
    let finish!: (value: PermissionModeStatus) => void;
    mocks.groupPermissionModeSet.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));
    const toast=vi.fn(), user=userEvent.setup();
    const view=render(<PermissionPicker groupId="group-a" onToast={toast} />);
    await user.click(screen.getByRole("button",{name:"专家团权限：请求批准"}));
    await user.click(screen.getByRole("menuitemradio",{name:/^自动判断/}));
    view.unmount();
    await act(async()=>{finish(modeStatus(undefined,{permissionMode:"auto",configuredPermissionMode:"auto"}));});
    expect(toast).not.toHaveBeenCalled();
    expect(usePermissionModeStore.getState().homeMode).toBe("ask");
  });

  it("组织策略广播后重新读取专家团的实际权限，不沿用旧的免审批状态", async () => {
    mocks.groupPermissionModeGet.mockResolvedValueOnce(modeStatus(undefined,{permissionMode:"always-approve",configuredPermissionMode:"always-approve"}))
      .mockResolvedValue(modeStatus(undefined,{locked:true,lockedReason:"组织已要求人工批准"}));
    render(<PermissionPicker groupId="group-a" groupPermissionMode="always-approve" />);
    await screen.findByRole("button",{name:"专家团权限：本专家团免审批"});
    await act(async()=>{usePermissionModeStore.getState().setStatus(modeStatus(undefined,{locked:true}));});
    await screen.findByRole("button",{name:"专家团权限：请求批准"});
    expect(mocks.groupPermissionModeGet).toHaveBeenCalledTimes(2);
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
  });

  it("菜单只保留简短说明，独立授权边界放在提权确认中", async () => {
    const user = userEvent.setup();
    render(<PermissionPicker />);

    await user.click(screen.getByRole("button", { name: /请求批准/ }));
    const item = screen.getByRole("menuitemradio", { name: /^本任务免审批/ });
    expect(item).toHaveTextContent("常规工具操作不再逐次询问");
    expect(item).not.toHaveTextContent("操作电脑");
  });

  it("通过 body portal 完整展示并根据视口定位，不受卡片裁剪", async () => {
    const user = userEvent.setup();
    render(
      <div data-testid="clipping-card" style={{ overflow: "hidden" }}>
        <PermissionPicker />
      </div>,
    );
    const trigger = screen.getByRole("button", { name: /请求批准/ });
    trigger.getBoundingClientRect = () => ({
      x: 760,
      y: 520,
      top: 520,
      right: 860,
      bottom: 552,
      left: 760,
      width: 100,
      height: 32,
      toJSON: () => ({}),
    });

    await user.click(trigger);

    const menu = screen.getByRole("menu", { name: "本任务权限" });
    expect(menu.parentElement).toBe(document.body);
    expect(menu).toHaveStyle({ position: "fixed" });
    expect(menu).toHaveAttribute("data-placement", "top");
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(trigger).toHaveAttribute("aria-controls", menu.id);
    expect(menu).toHaveTextContent("需要授权的操作由你确认");
    expect(menu).toHaveTextContent("仅限当前任务");

    await user.click(screen.getByRole("menuitemradio", { name: /^本任务免审批/ }));
    expect(screen.getByRole("alertdialog", { name: "确认本任务免审批" })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("alertdialog", { name: "确认本任务免审批" })).toBeNull();
    await waitFor(() => expect(screen.getByRole("menuitemradio", { name: /^本任务免审批/ })).toHaveFocus());

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "本任务权限" })).toBeNull();
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("方向键导航时跳过不可用模式，Escape 关闭并返回触发器", async () => {
    mocks.permissionModeGet.mockResolvedValue(modeStatus(undefined, { autoModeAvailable: false }));
    const user = userEvent.setup();
    render(<PermissionPicker />);
    await waitFor(() => expect(usePermissionModeStore.getState().capabilityStatus?.autoModeAvailable).toBe(false));
    const trigger = screen.getByRole("button", { name: "请求批准" });
    trigger.focus();
    await user.keyboard("{ArrowDown}");
    await waitFor(() => expect(screen.getByRole("menuitemradio", { name: /^请求批准/ })).toHaveFocus());
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitemradio", { name: /^本任务免审批/ })).toHaveFocus();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(usePermissionModeStore.getState().homeMode).toBe("ask");
  });

  it("切换任务会撤销尚未确认的提权选择", async () => {
    mocks.permissionModeGet.mockImplementation((id?: string) => Promise.resolve(modeStatus(id)));
    const user = userEvent.setup();
    const view = render(<PermissionPicker sessionId="session-1" />);
    await user.click(screen.getByRole("button", { name: "请求批准" }));
    await user.click(screen.getByRole("menuitemradio", { name: /^本任务免审批/ }));
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    view.rerender(<PermissionPicker sessionId="session-2" />);
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.queryByRole("menu")).toBeNull();
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
  });

  it("首页选择只修改待创建任务，不调用后端也不改其他任务", async () => {
    usePermissionModeStore.getState().setStatus(modeStatus("other", {
      permissionMode: "always-approve",
      configuredPermissionMode: "always-approve",
      runtimeAppliedMode: "always-approve",
    }));
    const onToast = vi.fn();
    const user = userEvent.setup();
    render(<PermissionPicker onToast={onToast} />);

    await waitFor(() => expect(mocks.permissionModeGet).toHaveBeenCalledWith(undefined));
    await user.click(screen.getByRole("button", { name: /请求批准/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /^自动判断/ }));

    expect(usePermissionModeStore.getState().homeMode).toBe("auto");
    expect(usePermissionModeStore.getState().statuses.other.permissionMode)
      .toBe("always-approve");
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
    expect(onToast).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "自动判断" })).toBeInTheDocument();
  });

  it("已有任务仅定向切换当前 session，已弹出的审批保持待处理", async () => {
    useSessionsStore.setState({ independent: [session("session-1"), session("session-2")] });
    usePermissionModeStore.getState().setStatus(modeStatus("session-2"));
    usePermissionStore.getState().request(permission("permission-1", "session-1"));
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1"));
    mocks.permissionModeSet.mockResolvedValue(setResult("session-1", "auto", {
      remainingPending: 1,
    }));
    const onToast = vi.fn();
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" onToast={onToast} />);

    await waitFor(() => expect(mocks.permissionModeGet).toHaveBeenCalledWith("session-1"));
    await user.click(screen.getByRole("button", { name: /请求批准/ }));
    expect(screen.getByText("仅限当前任务")).toBeInTheDocument();
    await user.click(screen.getByRole("menuitemradio", { name: /^自动判断/ }));

    await waitFor(() => expect(mocks.permissionModeSet)
      .toHaveBeenCalledWith("session-1", "auto"));
    expect(usePermissionModeStore.getState().statuses["session-1"].permissionMode).toBe("auto");
    expect(usePermissionModeStore.getState().statuses["session-2"].permissionMode).toBe("ask");
    expect(useSessionsStore.getState().independent.find((item) => item.sessionId === "session-1")?.permissionMode)
      .toBe("auto");
    expect(useSessionsStore.getState().independent.find((item) => item.sessionId === "session-2")?.permissionMode)
      .toBe("ask");
    expect(usePermissionStore.getState().queues["session-1"]).toHaveLength(1);
    expect(usePermissionStore.getState().closedRequestIds).toEqual([]);
    expect(onToast).toHaveBeenCalledWith(
      "仍有 1 项操作等待你确认",
    );
  });

  it("提高为本任务免审批前要求明确确认", async () => {
    useSessionsStore.setState({ independent: [session("session-1")] });
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1"));
    mocks.permissionModeSet.mockResolvedValue(setResult("session-1", "always-approve"));
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" />);

    await user.click(await screen.findByRole("button", { name: /请求批准/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /^本任务免审批/ }));

    const dialog = screen.getByRole("alertdialog", { name: "确认本任务免审批" });
    expect(within(dialog).getByText(/现有请求仍需单独确认/)).toBeInTheDocument();
    expect(within(dialog).getByText(/网页与电脑操作仍遵守各自的授权规则/)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "取消" })).toHaveFocus();
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole("button", { name: "确认开启" }));
    await waitFor(() => expect(mocks.permissionModeSet)
      .toHaveBeenCalledWith("session-1", "always-approve"));
  });

  it("下一动画帧前打开确认框，菜单的延迟聚焦不会抢走取消按钮焦点", async () => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame"] });
    const user = userEvent.setup();
    const view = render(<PermissionPicker />);
    try {
      const trigger = screen.getByRole("button", { name: "请求批准" });
      await user.click(trigger);
      const alwaysOption = screen.getByRole("menuitemradio", { name: /^本任务免审批/ });
      await user.click(alwaysOption);

      const dialog = screen.getByRole("alertdialog", { name: "确认本任务免审批" });
      const cancel = within(dialog).getByRole("button", { name: "取消" });
      expect(cancel).toHaveFocus();
      await act(async () => vi.advanceTimersToNextFrame());
      expect(cancel).toHaveFocus();
      expect(mocks.permissionModeSet).not.toHaveBeenCalled();
      expect(usePermissionModeStore.getState().homeMode).toBe("ask");

      await user.keyboard("{Escape}");
      await act(async () => vi.advanceTimersToNextFrame());
      expect(screen.queryByRole("alertdialog")).toBeNull();
      expect(alwaysOption).toHaveFocus();

      await user.keyboard("{Escape}");
      await act(async () => vi.advanceTimersToNextFrame());
      expect(screen.queryByRole("menu")).toBeNull();
      expect(trigger).toHaveFocus();
    } finally {
      view.unmount();
      vi.useRealTimers();
    }
  });

  it("电脑模式的边界在确认时说明，切换成功后不重复弹提示", async () => {
    useSessionsStore.setState({ independent: [session("session-1")] });
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1"));
    mocks.permissionModeSet.mockResolvedValue(setResult("session-1", "always-approve"));
    const { useSessionStore } = await import("@/stores/session-store");
    useSessionStore.setState((state) => ({
      ...state,
      transcripts: {
        ...state.transcripts,
        "session-1": {
          ...state.transcripts["session-1"],
          agentMode: "computer_use",
          messages: [],
          streamingMessageId: null,
          pendingSendNowPromptId: null,
          usage: {},
          plan: null,
          planMode: false,
          planApprovals: [],
          suppressReplay: false,
          dismissedControlPromptIds: [],
        },
      },
    }));
    const onToast = vi.fn();
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" onToast={onToast} />);

    await user.click(await screen.findByRole("button", { name: /请求批准/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /^本任务免审批/ }));
    await user.click(screen.getByRole("button", { name: "确认开启" }));

    await waitFor(() => expect(mocks.permissionModeSet).toHaveBeenCalledWith("session-1", "always-approve"));
    expect(onToast).not.toHaveBeenCalled();
  });

  it("组织策略锁定时展示原因并禁止任务修改", async () => {
    useSessionsStore.setState({ independent: [session("session-1")] });
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1", {
      locked: true,
      lockedReason: "权限模式已被组织策略锁定为 ask",
    }));
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" />);

    await waitFor(() => expect(usePermissionModeStore.getState().statuses["session-1"]?.locked)
      .toBe(true));
    await user.click(screen.getByRole("button", { name: /请求批准/ }));

    expect(screen.getByText("权限模式已被组织策略锁定为 ask")).toBeInTheDocument();
    expect(screen.getByRole("menuitemradio", { name: /^自动判断/ })).toBeDisabled();
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
  });

  it("项目默认权限独立于首页草稿，并显示组织锁定后的实际值", async () => {
    usePermissionModeStore.setState({ homeMode: "always-approve" });
    const { container } = render(<PermissionPicker inheritTaskDefault showLockedAsText />);
    expect(screen.getByRole("status")).toHaveTextContent("正在读取任务默认权限");
    expect(container.querySelector(".permission-picker__value")).toBeNull();
    await waitFor(() => expect(container.querySelector(".permission-picker__value")).toHaveTextContent("请求批准"));
    expect(selectProjectPermissionMode(usePermissionModeStore.getState())).toBe("ask");
    await act(async () => usePermissionModeStore.getState().setStatus(modeStatus(undefined, {
      locked: true, permissionMode: "auto", configuredPermissionMode: "auto", lockedReason: "组织统一使用自动判断",
    })));
    expect(screen.getByRole("button", { name: /自动判断.*组织锁定/ })).toBeVisible();
    expect(selectProjectPermissionMode(usePermissionModeStore.getState())).toBe("auto");
    expect(usePermissionModeStore.getState().homeMode).toBe("always-approve");
  });

  it("项目权限读取失败时隐藏旧预览，重试成功后恢复发送所需的模式", async () => {
    const user = userEvent.setup();
    usePermissionModeStore.setState({ capabilityStatus: modeStatus() });
    mocks.permissionModeGet.mockRejectedValueOnce(new Error("Runtime 未连接"));
    render(<PermissionPicker inheritTaskDefault showLockedAsText />);
    expect(screen.queryByText("请求批准")).toBeNull();
    expect(selectProjectPermissionMode(usePermissionModeStore.getState())).toBeUndefined();
    const retry = await screen.findByRole("button", { name: "重试读取权限" });
    expect(screen.getByRole("status")).toHaveTextContent("Runtime 未连接");
    await user.tab();
    expect(retry).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(await screen.findByText("请求批准")).toBeVisible();
    expect(screen.getByRole("group", { name: "任务默认权限" })).toHaveFocus();
    expect(selectProjectPermissionMode(usePermissionModeStore.getState())).toBe("ask");
  });

  it("重试后仅在权限设置仍拥有焦点时聚焦锁定说明", async () => {
    const user = userEvent.setup();
    let complete: (status: PermissionModeStatus) => void = () => {};
    mocks.permissionModeGet.mockRejectedValueOnce(new Error("读取失败"))
      .mockImplementationOnce(() => new Promise<PermissionModeStatus>(resolve => { complete = resolve; }));
    render(<><PermissionPicker inheritTaskDefault showLockedAsText /><button>其他设置</button></>);
    await user.click(await screen.findByRole("button", { name: "重试读取权限" }));
    expect(screen.getByRole("group", { name: "任务默认权限" })).toHaveFocus();
    const other = screen.getByRole("button", { name: "其他设置" });
    await user.click(other);
    await act(async () => complete(modeStatus(undefined, { locked: true })));
    expect(screen.getByRole("button", { name: /请求批准.*组织锁定/ })).toBeVisible();
    expect(other).toHaveFocus();
  });

  it("组织要求的项目权限不可用时不会把降级状态当成可创建的预览", async () => {
    mocks.permissionModeGet.mockResolvedValue(modeStatus(undefined, {
      locked: true, configuredPermissionMode: "auto", autoModeAvailable: false,
      autoModeUnavailableReason: "组织要求自动判断，但分类器不可用",
    }));
    render(<PermissionPicker inheritTaskDefault showLockedAsText />);
    expect(await screen.findByText("组织要求自动判断，但分类器不可用")).toBeVisible();
    expect(screen.queryByText("请求批准")).toBeNull();
    expect(selectProjectPermissionMode(usePermissionModeStore.getState())).toBeUndefined();
  });

  it("会话权限的组织锁定和离线原因可通过键盘展开", async () => {
    const user = userEvent.setup();
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1", {
      permissionMode: "auto", locked: true, lockedReason: "组织统一使用自动判断",
      runtimeSyncState: "offline", runtimeSyncError: "Runtime 尚未连接",
    }));
    render(<PermissionPicker sessionId="session-1" showLockedAsText />);
    const reason = await screen.findByRole("button", { name: /自动判断.*组织锁定/ });
    await user.tab();
    expect(reason).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(reason).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("组织统一使用自动判断。Runtime 尚未连接")).toBeVisible();
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
  });

  it("任务尚未加载进 Runtime 时展示状态并禁止权限切换", async () => {
    useSessionsStore.setState({ independent: [session("session-1")] });
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1", {
      runtimeSyncState: "offline",
      runtimeAppliedMode: undefined,
    }));
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" />);

    await waitFor(() => expect(usePermissionModeStore.getState().statuses["session-1"])
      .toBeDefined());
    await user.click(screen.getByRole("button", { name: /请求批准/ }));

    expect(screen.getByRole("status")).toHaveTextContent(
      "当前任务正在加载，权限模式暂不可切换",
    );
    expect(screen.getByRole("menuitemradio", { name: /^自动判断/ })).toBeDisabled();
    expect(mocks.permissionModeSet).not.toHaveBeenCalled();
  });

  it("能力变化会将首页不再可用的草稿权限安全回退为请求批准", async () => {
    usePermissionModeStore.setState({ homeMode: "auto" });
    mocks.permissionModeGet.mockResolvedValue(modeStatus(undefined, {
      autoModeAvailable: false,
      autoModeUnavailableReason: "自动判断已被组织策略关闭",
    }));
    render(<PermissionPicker />);

    await waitFor(() => expect(usePermissionModeStore.getState().homeMode).toBe("ask"));
    expect(screen.getByRole("button", { name: /请求批准/ })).toBeInTheDocument();
  });

  it("延迟的初始读取不会覆盖用户刚完成的任务切换", async () => {
    let resolveInitialRead: ((value: PermissionModeStatus) => void) | undefined;
    useSessionsStore.setState({ independent: [session("session-1")] });
    mocks.permissionModeGet.mockImplementation(() => new Promise((resolve) => {
      resolveInitialRead = resolve;
    }));
    mocks.permissionModeSet.mockResolvedValue(setResult("session-1", "auto"));
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" />);

    await user.click(screen.getByRole("button", { name: /请求批准/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /^自动判断/ }));
    await waitFor(() => expect(usePermissionModeStore.getState().statuses["session-1"]?.permissionMode)
      .toBe("auto"));

    await act(async () => resolveInitialRead?.(modeStatus("session-1")));
    expect(usePermissionModeStore.getState().statuses["session-1"].permissionMode).toBe("auto");
  });

  it("后端拒绝切换时保留原权限并给出可理解的错误", async () => {
    useSessionsStore.setState({ independent: [session("session-1")] });
    mocks.permissionModeGet.mockResolvedValue(modeStatus("session-1"));
    mocks.permissionModeSet.mockRejectedValue(new Error("自动判断已被组织策略关闭"));
    const onToast = vi.fn();
    const user = userEvent.setup();
    render(<PermissionPicker sessionId="session-1" onToast={onToast} />);

    await waitFor(() => expect(usePermissionModeStore.getState().statuses["session-1"])
      .toBeDefined());
    await user.click(screen.getByRole("button", { name: /请求批准/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /^自动判断/ }));

    await waitFor(() => expect(onToast)
      .toHaveBeenCalledWith("权限模式切换失败：自动判断已被组织策略关闭"));
    expect(useSessionsStore.getState().independent[0].permissionMode).toBe("ask");
    expect(usePermissionModeStore.getState().statuses["session-1"].permissionMode).toBe("ask");
  });
});
