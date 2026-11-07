// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(), agentsList: vi.fn(), skillsList: vi.fn(), mcpList: vi.fn(), filesystemPickFiles: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@/lib/agent-client", async (importOriginal) => ({
  ...await importOriginal<typeof import("@/lib/agent-client")>(),
  agentsList: mocks.agentsList,
  skillsList: mocks.skillsList,
  mcpList: mocks.mcpList,
  filesystemPickFiles: mocks.filesystemPickFiles,
}));

import { Composer } from "../Composer";

const base = {
  streaming: false, apiReady: true, onSend: vi.fn(), onCancel: vi.fn(), onOpenMeetingMinutes: vi.fn(),
};
const models = [
  { id: "organization/MiniMax-M3", remoteModelId: "MiniMax-M3", providerId: "echoagent-organization", source: "organization" as const, providerKind: "custom" },
  { id: "personal/other", remoteModelId: "other-chat", providerId: "personal", source: "personal" as const, providerKind: "custom" },
  { id: "echoagent-ojlab/MiniMax-M3", remoteModelId: "MiniMax-M3", providerId: "echoagent-ojlab", source: "legacy" as const, providerKind: "custom" },
];
// The before run accepts the old wording: failures must identify lost access.
const meetingLabel = /录音转写|会议录音与纪要/;

describe("Composer meeting history entry", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.invoke.mockResolvedValue(undefined);
    mocks.agentsList.mockResolvedValue([]);
    mocks.skillsList.mockResolvedValue([]);
    mocks.mcpList.mockResolvedValue([]);
    mocks.filesystemPickFiles.mockResolvedValue([]);
  });

  it("opens history while speech capability is pending, unsupported, or the selected model is ineligible", async () => {
    let rejectProbe!: (error: Error) => void;
    const probe = new Promise<void>((_resolve, reject) => { rejectProbe = reject; });
    mocks.invoke.mockImplementation((command: string) => command === "meeting_check_connection"
      ? probe : Promise.resolve(undefined));
    const { rerender } = render(<Composer {...base} modelId={models[0].id} models={models} modelLoading />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "添加" })); });
    fireEvent.click(screen.getByRole("menuitem", { name: meetingLabel }));
    expect(base.onOpenMeetingMinutes).toHaveBeenCalledTimes(1);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_check_connection", {
      modelId: models[0].id, providerId: models[0].providerId,
    });
    await act(async () => { rejectProbe(new Error("当前端点不支持语音转写")); });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "添加" })); });
    fireEvent.click(screen.getByRole("menuitem", { name: meetingLabel }));
    expect(base.onOpenMeetingMinutes).toHaveBeenCalledTimes(2);
    rerender(<Composer {...base} modelId={models[1].id} models={models} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "添加" })); });
    fireEvent.click(screen.getByRole("menuitem", { name: meetingLabel }));
    expect(base.onOpenMeetingMinutes).toHaveBeenCalledTimes(3);
    expect(mocks.invoke.mock.calls.filter(([command]) => command === "meeting_check_connection")).toHaveLength(1);
  });

  it("with no ready chat service allows only history navigation through the real menu", async () => {
    const onSelectExpert = vi.fn();
    const onSelectSkill = vi.fn();
    const onNavigateConnectors = vi.fn();
    const onAutomationModeChange = vi.fn();
    render(<Composer {...base} apiReady={false} modelLoading models={[]}
      onSelectExpert={onSelectExpert} onSelectSkill={onSelectSkill}
      onNavigateConnectors={onNavigateConnectors} onAutomationModeChange={onAutomationModeChange} />);
    expect(screen.getByRole("textbox")).toBeDisabled();
    const trigger = screen.getByRole("button", { name: "添加" });
    expect(trigger).toBeEnabled();
    await act(async () => { fireEvent.click(trigger); });
    for (const name of ["添加文件", "专家", "技能", "连接器"]) {
      const item = screen.getByRole("menuitem", { name });
      expect(item).toBeDisabled();
      fireEvent.click(item);
      fireEvent.keyDown(item, { key: "ArrowRight" });
    }
    for (const name of [/操作网页/, /操作电脑/]) {
      const item = screen.getByRole("menuitemradio", { name });
      expect(item).toBeDisabled();
      fireEvent.click(item);
    }
    expect(screen.queryByRole("menu", { name: /子菜单/ })).toBeNull();
    expect(mocks.agentsList).not.toHaveBeenCalled();
    expect(mocks.skillsList).not.toHaveBeenCalled();
    expect(mocks.mcpList).not.toHaveBeenCalled();
    expect(mocks.filesystemPickFiles).not.toHaveBeenCalled();
    expect(onSelectExpert).not.toHaveBeenCalled();
    expect(onSelectSkill).not.toHaveBeenCalled();
    expect(onNavigateConnectors).not.toHaveBeenCalled();
    expect(onAutomationModeChange).not.toHaveBeenCalled();
    const meeting = screen.getByRole("menuitem", { name: meetingLabel });
    expect(meeting).toBeEnabled();
    fireEvent.keyDown(meeting, { key: "Home" });
    expect(meeting).toHaveFocus();
    fireEvent.click(meeting);
    expect(base.onOpenMeetingMinutes).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu", { name: "添加内容" })).toBeNull();
  });

  it("without a history callback does not expose an unusable entry or bypass chat readiness", async () => {
    const { rerender } = render(<Composer {...base} onOpenMeetingMinutes={undefined} models={[]} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "添加" })); });
    expect(screen.queryByRole("menuitem", { name: meetingLabel })).toBeNull();
    rerender(<Composer {...base} onOpenMeetingMinutes={undefined} apiReady={false} models={[]} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "添加" })).toBeDisabled());
    expect(screen.queryByRole("menu", { name: "添加内容" })).toBeNull();
  });

  it.each(["disabled", "awaitingQuestion"] as const)("preserves the explicit %s interaction guard", (guard) => {
    render(<Composer {...base} apiReady={false} {...{ [guard]: true }} />);
    const trigger = screen.getByRole("button", { name: "添加" });
    expect(trigger).toBeDisabled();
    fireEvent.click(trigger);
    expect(screen.queryByRole("menu", { name: "添加内容" })).toBeNull();
    expect(base.onOpenMeetingMinutes).not.toHaveBeenCalled();
  });
});
