// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  checkConnection: vi.fn(),
  meetingList: vi.fn(),
  captureSupport: vi.fn(),
  recorderStart: vi.fn(),
  retryTranscription: vi.fn(),
  meetingGet: vi.fn(),
  meetingUpdate: vi.fn(),
  meetingJobActive: vi.fn(),
  meetingProcess: vi.fn(),
  meetingRegenerateMinutes: vi.fn(),
  listener: null as null | ((snapshot: unknown) => void),
}));

vi.mock("@/lib/agent-client", () => ({ filesystemPickFiles: vi.fn() }));
vi.mock("@/lib/meeting-minutes", () => ({
  MAX_MEETING_RECORDING_SAMPLES: 16_000 * 60 * 60 * 24,
  meetingCheckConnection: mocks.checkConnection,
  isMiniMaxMeetingModel: (model: { id: string; remoteModelId?: string }) =>
    /minimax/i.test(model.remoteModelId || model.id),
  meetingList: mocks.meetingList,
  meetingCaptureSupport: mocks.captureSupport,
  meetingGet: mocks.meetingGet,
  meetingUpdate: mocks.meetingUpdate,
  meetingJobActive: mocks.meetingJobActive,
  meetingProcess: mocks.meetingProcess,
  meetingRegenerateMinutes: mocks.meetingRegenerateMinutes,
  meetingRecorder: {
    current: () => ({ meeting: null, active: false, paused: false, level: 0, error: null, source: null }),
    subscribe: (listener: (snapshot: unknown) => void) => { mocks.listener = listener; return () => { mocks.listener = null; }; },
    syncNative: vi.fn().mockResolvedValue(undefined),
    start: mocks.recorderStart,
    retryTranscription: mocks.retryTranscription,
  },
  formatMeetingDuration: () => "00:00:00",
}));

vi.mock("../MeetingAudioPlayer", () => ({ MeetingAudioPlayer: () => null }));

import { MeetingMinutesPanel } from "../MeetingMinutesPanel";
import { meetingDrafts } from "@/lib/meeting-drafts";

beforeEach(() => meetingDrafts.clear());

const models = [
  { id: "personal/MiniMax-M3", label: "个人 MiniMax", providerId: "personal", providerKind: "custom", source: "personal" as const },
  { id: "organization/MiniMax-M3", label: "组织 MiniMax", providerId: "echoagent-organization", providerKind: "custom", source: "organization" as const, insecureHttp: true },
  { id: "echoagent-ojlab/MiniMax-M3", label: "内置 MiniMax", providerId: "echoagent-ojlab", providerKind: "custom", source: "legacy" as const },
];

describe("MeetingMinutesPanel recording first workflow", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.removeItem("echo.meeting.recording-source");
    mocks.meetingList.mockResolvedValue([]);
    mocks.checkConnection.mockRejectedValue(new Error("离线"));
    mocks.captureSupport.mockResolvedValue({ systemAudio: true });
    mocks.meetingJobActive.mockResolvedValue(false);
    vi.spyOn(window, "confirm").mockReturnValue(true);
  });
  it("does not expose import or service configuration and permits local recording offline", async () => {
    render(<MeetingMinutesPanel modelId={models[0].id} models={models} />);
    expect(await screen.findByRole("button", { name: "开始录音" })).toBeEnabled();
    expect(screen.queryByRole("combobox")).toBeNull();
    expect(screen.queryByRole("button", { name: /导入/ })).toBeNull();
    expect(mocks.checkConnection).not.toHaveBeenCalled();
  });
  it("can record without any configured model", async () => {
    mocks.recorderStart.mockResolvedValue({ id: "local", title: "离线录音", transcript: [], chunks: [], status: "recording", recordedSamples: 0 });
    mocks.meetingList.mockResolvedValue([]);
    render(<MeetingMinutesPanel models={[]} />);
    fireEvent.click(await screen.findByRole("button", { name: "开始录音" }));
    await waitFor(() => expect(mocks.recorderStart).toHaveBeenCalledWith(expect.any(String), "", "", "microphone"));
  });
  it('provides a settings route without blocking local recording when no connection exists',async()=>{
    const settings=vi.fn();render(<MeetingMinutesPanel models={[]} onOpenModelSettings={settings}/>);
    fireEvent.click(await screen.findByRole('button',{name:'配置转写连接'}));
    expect(settings).toHaveBeenCalledOnce();expect(screen.getByRole('button',{name:'开始录音'})).toBeEnabled();
  });
  it('shows the real credential error and retries a repaired binding while recording',async()=>{
    const settings=vi.fn();mocks.retryTranscription.mockResolvedValue(undefined);
    render(<MeetingMinutesPanel models={models} modelId={models[0].id} onOpenModelSettings={settings}/>);
    await screen.findByRole('button',{name:'开始录音'});
    await act(async()=>mocks.listener?.({active:true,paused:false,level:.1,source:'both',error:null,
      transcription:{state:'unavailable',error:'会议模型 API Key 未配置或已失效'},
      meeting:{id:'live-settings',title:'正在录音',modelId:'',providerId:'',status:'recording',createdAt:0,recordedSamples:64000,durationSeconds:4,transcript:[],chunks:[]}}));
    expect(screen.getByText(/转写连接需要处理/)).toBeInTheDocument();
    expect(screen.getByText('会议模型 API Key 未配置或已失效')).toBeInTheDocument();
    expect(screen.queryByText(/网络恢复后会补转/)).toBeNull();
    fireEvent.click(screen.getByRole('button',{name:'检查模型连接'}));expect(settings).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button',{name:'重试转写'}));
    await waitFor(()=>expect(mocks.retryTranscription).toHaveBeenCalledWith({modelId:models[0].id,providerId:'personal'}));
  });
  it("passes the mixed audio source and current compatible binding without a model selector", async () => {
    const record = { id: "record-1", title: "测试会议", status: "recording", transcript: [], chunks: [], recordedSamples: 0 };
    mocks.recorderStart.mockResolvedValue(record);
    render(<MeetingMinutesPanel modelId={models[0].id} models={models} />);
    const both = await screen.findByRole("button", { name: /两者都录/ });
    await waitFor(() => expect(both).toBeEnabled()); fireEvent.click(both);
    fireEvent.click(screen.getByRole("button", { name: "开始录音" }));
    await waitFor(() => expect(mocks.recorderStart).toHaveBeenCalledWith(expect.any(String), models[0].id, "personal", "both"));
  });
  it("retains a saved valid binding for retry", async () => {
    const record = { id: "failed-binding", title: "需要恢复的会议", createdAt: 0, status: "failed", modelId: models[1].id, providerId: models[1].providerId, transcript: [], chunks: [], recordedSamples: 16000, durationSeconds: 1 };
    mocks.meetingList.mockResolvedValue([record]); mocks.meetingProcess.mockResolvedValue(record);
    render(<MeetingMinutesPanel modelId={models[0].id} models={models} />);
    fireEvent.click(await screen.findByRole("button", { name: "重试未完成部分" }));
    await waitFor(() => expect(mocks.meetingProcess).toHaveBeenCalledWith(record.id, { modelId: models[1].id, providerId: models[1].providerId }));
  });
  it("falls back to the current compatible binding when the saved connection was deleted", async () => {
    const record = { id: "deleted-binding", title: "连接已删除", createdAt: 0, status: "completed", modelId: "deleted/MiniMax", providerId: "deleted", minutes: "# 原纪要", transcript: [{ id: 0, start: 0, end: 1, text: "已有转写", speaker: "说话人1" }], chunks: [], recordedSamples: 16000, durationSeconds: 1 };
    mocks.meetingList.mockResolvedValue([record]); mocks.meetingRegenerateMinutes.mockResolvedValue(record);
    render(<MeetingMinutesPanel modelId={models[0].id} models={models} />);
    await screen.findByRole("heading", { name: record.title });
    fireEvent.click(screen.getByRole("button", { name: "更多录音操作" }));
    fireEvent.click(screen.getByRole("button", { name: "重新生成纪要" }));
    await waitFor(() => expect(mocks.meetingRegenerateMinutes).toHaveBeenCalledWith(record.id, { modelId: models[0].id, providerId: "personal" }));
  });
  it("generates minutes from saved text without a speech capability probe", async () => {
    const record = { id: "text-only", title: "已有原文", createdAt: 0, status: "transcribed", transcript: [{ id: 0, start: 0, end: 1, text: "已有转写", speaker: "说话人1" }], chunks: [], recordedSamples: 16000, durationSeconds: 1 };
    mocks.meetingList.mockResolvedValue([record]); mocks.meetingRegenerateMinutes.mockResolvedValue(record);
    render(<MeetingMinutesPanel modelId={models[0].id} models={models} />);
    await screen.findByRole("heading", { name: record.title });
    fireEvent.click(screen.getByRole("tab", { name: "会议纪要" }));
    fireEvent.click(screen.getByRole("button", { name: "生成会议纪要" }));
    await waitFor(() => expect(mocks.meetingRegenerateMinutes).toHaveBeenCalledWith(record.id, { modelId: models[0].id, providerId: "personal" }));
  });
  it("resumes an unfinished saved recording before generating a summary of its partial transcript", async () => {
    const record = { id: "exit-recovery", title: "退出后恢复", createdAt: 0, status: "recorded", transcript: [{ id:0,start:0,end:4,speaker:"S1",text:"仅识别了前半段" }], live: { processedSamples:64000 }, chunks: [], recordedSamples:128000, durationSeconds:8 };
    mocks.meetingList.mockResolvedValue([record]);mocks.meetingProcess.mockResolvedValue(record);
    render(<MeetingMinutesPanel models={models}/>);
    fireEvent.click(await screen.findByRole("button",{name:"继续转写并整理纪要"}));
    await waitFor(()=>expect(mocks.meetingProcess).toHaveBeenCalledWith(record.id,expect.any(Object)));
    expect(mocks.meetingRegenerateMinutes).not.toHaveBeenCalled();
  });
});

describe("meeting content preservation", () => {
  const historic = { id: "historic", title: "历史会议", createdAt: 0, status: "completed", minutes: "# 原纪要", transcript: [{ id: 0, start: 0, end: 1, text: "原转写", speaker: "说话人1" }], chunks: [], recordedSamples: 16000, durationSeconds: 1 };
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.meetingList.mockResolvedValue([historic]);
    mocks.checkConnection.mockResolvedValue(undefined);
    mocks.meetingJobActive.mockResolvedValue(false);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    mocks.captureSupport.mockResolvedValue({ systemAudio: true });
  });
  it("retains historical selection and unsaved transcription during recorder updates", async () => {
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: "历史会议" });
    fireEvent.click(screen.getByRole("tab", { name: "转写原文" }));
    if (!screen.queryByRole("textbox", { name: "第 1 段转写" })) {
      const edit = screen.getByRole("button", { name: "校对原文" });
      await waitFor(() => expect(edit).toBeEnabled()); fireEvent.click(edit);
    }
    fireEvent.change(screen.getByRole("textbox", { name: "第 1 段转写" }), { target: { value: "校对草稿" } });
    act(() => mocks.listener?.({ meeting: { ...historic, id: "active", title: "活动会议", status: "recording" }, active: true, paused: false, level: 0.8, error: null, source: "microphone" }));
    expect(screen.getByRole("heading", { name: "历史会议" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "第 1 段转写" })).toHaveValue("校对草稿");
  });
  it("keeps processing polls running while another meeting emits frequent recording levels", async () => {
    vi.useFakeTimers();
    const processing = { ...historic, status: "transcribing", minutes: undefined };
    mocks.meetingList.mockResolvedValue([processing]);
    mocks.meetingGet.mockResolvedValue(processing);
    mocks.meetingJobActive.mockResolvedValue(true);
    const view = render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    try {
      await act(async () => { await Promise.resolve(); await Promise.resolve(); });
      for (let index = 0; index < 20; index += 1) {
        await act(async () => {
          mocks.listener?.({ meeting: { ...historic, id: "other-active", status: "recording" }, active: true, paused: false, level: index / 20, error: null, source: "microphone" });
          await vi.advanceTimersByTimeAsync(100);
        });
      }
      expect(mocks.meetingGet).toHaveBeenCalledWith("historic");
    } finally {
      view.unmount();
      vi.useRealTimers();
    }
  });
  it("preserves a minutes draft after leaving and returning to the page", async () => {
    const view = render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: "历史会议" });
    fireEvent.click(screen.getByRole("button", { name: "编辑纪要" }));
    fireEvent.change(screen.getByRole("textbox", { name: "编辑会议纪要" }), { target: { value: "跨页面草稿" } });
    view.unmount();
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: "历史会议" });
    fireEvent.click(screen.getByRole("button", { name: "编辑纪要" }));
    expect(screen.getByRole("textbox", { name: "编辑会议纪要" })).toHaveValue("跨页面草稿");
  });
  it("saves edited minutes through the native persistence command", async () => {
    const edited = { ...historic, minutes: "# 已校对纪要" };
    mocks.meetingUpdate.mockResolvedValue(edited);
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: "历史会议" });
    fireEvent.click(screen.getByRole("button", { name: "编辑纪要" }));
    fireEvent.change(screen.getByRole("textbox", { name: "编辑会议纪要" }), { target: { value: edited.minutes } });
    mocks.meetingList.mockResolvedValue([edited]);
    fireEvent.click(screen.getByRole("button", { name: "保存纪要" }));
    await waitFor(() => expect(mocks.meetingUpdate).toHaveBeenCalledWith("historic", expect.objectContaining({ minutes: edited.minutes,
      expectedContent: { transcript: historic.transcript, minutes: historic.minutes } })));
    await waitFor(() => expect(screen.queryByRole("textbox", { name: "编辑会议纪要" })).toBeNull());
    expect(screen.getByText("已校对纪要")).toBeInTheDocument();
  });

  it("重试部分转写后合并新增段落及原校对，并提交完整内容", async () => {
    const partial = { ...historic, id: "partial-retry", status: "partial", minutes: undefined };
    const completed = { ...partial, status: "completed", minutes: "# 新纪要", transcript: [
      ...partial.transcript, { id: 1, start: 1, end: 2, text: "新增转写", speaker: "说话人2" },
    ] };
    mocks.meetingList.mockResolvedValue([partial]);
    mocks.meetingJobActive.mockResolvedValue(false);
    mocks.meetingProcess.mockImplementationOnce(async () => {
      mocks.meetingList.mockResolvedValue([completed]); return completed;
    });
    mocks.meetingUpdate.mockResolvedValue({ ...completed, minutes: undefined });
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: partial.title });
    fireEvent.click(screen.getByRole("tab", { name: "转写原文" }));
    if (!screen.queryByRole("textbox", { name: "第 1 段转写" })) {
      const edit = screen.getByRole("button", { name: "校对原文" });
      await waitFor(() => expect(edit).toBeEnabled()); fireEvent.click(edit);
    }
    fireEvent.change(screen.getByRole("textbox", { name: "第 1 段转写" }), { target: { value: "人工校对" } });
    fireEvent.click(screen.getByRole("button", { name: "重试未完成部分" }));
    await screen.findByText("新纪要");
    fireEvent.click(screen.getByRole("button", { name: /^(保存并导出|导出)$/ }));
    expect(screen.getByRole("button", { name: "Word" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "更多录音操作" }));
    expect(screen.getByRole("button", { name: "重新生成纪要" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "编辑纪要" })).toBeDisabled();
    expect(screen.getByText(/原文校对尚未保存/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "转写原文" }));
    if (!screen.queryByRole("textbox", { name: "第 1 段转写" })) {
      const edit = screen.getByRole("button", { name: "校对原文" });
      await waitFor(() => expect(edit).toBeEnabled()); fireEvent.click(edit);
    }
    expect(screen.getByRole("textbox", { name: "第 1 段转写" })).toHaveValue("人工校对");
    expect(screen.getByRole("textbox", { name: "第 2 段转写" })).toHaveValue("新增转写");
    fireEvent.click(screen.getByRole("button", { name: "保存校对" }));
    await waitFor(() => expect(mocks.meetingUpdate).toHaveBeenCalledWith(partial.id, {
      transcript: [{ ...completed.transcript[0], text: "人工校对" }, completed.transcript[1]],
      expectedContent: { transcript: completed.transcript, minutes: completed.minutes },
    }));
  });

  it("历史记录已保存纪要但仍有失败分片时继续提供转写重试", async () => {
    mocks.meetingList.mockResolvedValue([{ ...historic, status: "completed", chunks: [
      { index: 0, start: 0, end: 1, status: "completed" },
      { index: 1, start: 1, end: 2, status: "failed", error: "连接中断" },
    ] }]);
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: historic.title });
    await waitFor(() => expect(screen.getByRole("button", { name: "重试未完成部分" })).toBeEnabled());
  });

  it("重新生成纪要后显示新预览和草稿冲突，明确选择后才编辑旧草稿", async () => {
    const original = { ...historic, id: "regenerated-draft" };
    const regenerated = { ...original, minutes: "# 最新纪要" };
    mocks.meetingList.mockResolvedValue([original]);
    mocks.meetingRegenerateMinutes.mockImplementationOnce(async () => {
      mocks.meetingList.mockResolvedValue([regenerated]); return regenerated;
    });
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: original.title });
    fireEvent.click(screen.getByRole("button", { name: "编辑纪要" }));
    fireEvent.change(screen.getByRole("textbox", { name: "编辑会议纪要" }), { target: { value: "旧版人工纪要" } });
    fireEvent.click(screen.getByRole("button", { name: /^(保存并导出|导出)$/ }));
    expect(screen.getByRole("button", { name: "Word" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "返回阅读（保留草稿）" }));
    fireEvent.click(screen.getByRole("button", { name: "更多录音操作" }));
    fireEvent.click(screen.getByRole("button", { name: "重新生成纪要" }));
    await screen.findByText("最新纪要");
    expect(screen.getByRole("alert")).toHaveTextContent("原纪要草稿已保留");
    expect(screen.getByRole("button", { name: "编辑纪要" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "保留原草稿并编辑" }));
    expect(screen.getByRole("textbox", { name: "编辑会议纪要" })).toHaveValue("旧版人工纪要");
    fireEvent.click(screen.getByRole("button", { name: "返回阅读（保留草稿）" }));
    expect(screen.getByText("最新纪要")).toBeInTheDocument();
  });

  it("保存前内容已变化时重新读取并保留冲突草稿，不自动覆盖", async () => {
    const original = { ...historic, id: "changed-on-save" };
    const latest = { ...original, transcript: [{ ...historic.transcript[0], text: "最新服务端转写" }] };
    mocks.meetingList.mockResolvedValue([original]);
    mocks.meetingUpdate.mockImplementationOnce(async () => {
      mocks.meetingList.mockResolvedValue([latest]);
      throw new Error("会议内容已更新，请刷新后合并校对再保存");
    });
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: original.title });
    fireEvent.click(screen.getByRole("tab", { name: "转写原文" }));
    if (!screen.queryByRole("textbox", { name: "第 1 段转写" })) {
      const edit = screen.getByRole("button", { name: "校对原文" });
      await waitFor(() => expect(edit).toBeEnabled()); fireEvent.click(edit);
    }
    fireEvent.change(screen.getByRole("textbox", { name: "第 1 段转写" }), { target: { value: "人工校对" } });
    fireEvent.click(screen.getByRole("button", { name: "保存校对" }));
    await screen.findByText(/最新转写与原校对草稿有冲突/);
    expect(screen.getByRole("button", { name: "保存校对" })).toBeDisabled();
    expect(screen.getByRole("textbox", { name: "第 1 段转写" })).toHaveValue("人工校对");
    expect(screen.getByRole("textbox", { name: "第 1 段转写" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "舍弃校对，使用最新转写" }));
    expect(screen.getByRole("textbox", { name: "第 1 段转写" })).toHaveValue("最新服务端转写");
    expect(screen.getByRole("textbox", { name: "第 1 段转写" })).toBeEnabled();
    expect(mocks.meetingUpdate).toHaveBeenCalledTimes(1);
  });

  it("保存期间锁定校对输入，写入失败后恢复编辑并保留内容", async () => {
    let failSave!: (error: Error) => void;
    mocks.meetingUpdate.mockReturnValueOnce(new Promise((_, reject) => { failSave = reject; }));
    render(<MeetingMinutesPanel models={models.slice(0, 1)} />);
    await screen.findByRole("heading", { name: historic.title });
    fireEvent.click(screen.getByRole("tab", { name: "转写原文" }));
    if (!screen.queryByRole("textbox", { name: "第 1 段转写" })) {
      const edit = screen.getByRole("button", { name: "校对原文" });
      await waitFor(() => expect(edit).toBeEnabled()); fireEvent.click(edit);
    }
    const input = screen.getByRole("textbox", { name: "第 1 段转写" });
    fireEvent.change(input, { target: { value: "不能丢失的校对" } });
    fireEvent.click(screen.getByRole("button", { name: "保存校对" }));
    expect(input).toBeDisabled();
    await act(async () => failSave(new Error("磁盘已满")));
    await waitFor(() => expect(input).toBeEnabled());
    expect(input).toHaveValue("不能丢失的校对");
  });
});
