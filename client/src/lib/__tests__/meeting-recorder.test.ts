// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

const record = {
  id: "meeting-1", title: "产品会议", status: "recording", modelId: "MiniMax", providerId: "personal",
  captureSource: "both", recordedSamples: 0, durationSeconds: 0, transcript: [], chunks: [],
};

describe("meeting processing model binding", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.invoke.mockResolvedValue(record);
  });
  it("sends the selected connection for processing and regeneration", async () => {
    const { meetingProcess, meetingRegenerateMinutes } = await import("../meeting-minutes");
    const binding = { modelId: "replacement/MiniMax", providerId: "replacement" };
    await meetingProcess(record.id, binding);
    await meetingRegenerateMinutes(record.id, binding);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_process", { meetingId: record.id, ...binding });
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_regenerate_minutes", { meetingId: record.id, ...binding });
  });
  it("keeps legacy callers on the saved meeting binding", async () => {
    const { meetingProcess } = await import("../meeting-minutes");
    await meetingProcess(record.id);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_process", expect.objectContaining({ meetingId: record.id }));
    const args = mocks.invoke.mock.calls[0][1];
    expect(args.modelId ?? null).toBeNull();
    expect(args.providerId ?? null).toBeNull();
  });
});

describe("native meeting recorder", () => {
  beforeEach(() => {
    vi.resetModules();
    mocks.invoke.mockReset();
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") return { meetingId: record.id, active: true, paused: false, level: 0, recordedSamples: 0 };
      if (command === "meeting_capture_pause") return { ...record, status: "paused" };
      if (command === "meeting_capture_stop") return { ...record, status: "recorded", recordedSamples: 16_000, durationSeconds: 1 };
      return undefined;
    });
  });

  it("starts, pauses and saves a mixed system and microphone recording", async () => {
    const { meetingRecorder } = await import("../meeting-minutes");
    await meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_create", expect.objectContaining({ captureSource: "both" }));
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_capture_start", { meetingId: record.id, mode: "both" });
    expect(meetingRecorder.current()).toMatchObject({ active: true, source: "both" });

    await meetingRecorder.setPaused(true);
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_capture_pause", { meetingId: record.id, paused: true });
    expect(meetingRecorder.current().paused).toBe(true);

    const saved = await meetingRecorder.stop();
    expect(saved.status).toBe("recorded");
    expect(meetingRecorder.current()).toMatchObject({ active: false, source: null });
  });

  it("automatically finalizes native capture once the persisted sample limit is reached", async () => {
    const { meetingRecorder, MAX_MEETING_RECORDING_SAMPLES } = await import("../meeting-minutes");
    const capped = { ...record, recordedSamples: MAX_MEETING_RECORDING_SAMPLES, durationSeconds: 86400 };
    let finished = false;
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") return { meetingId: record.id, active: true, recordedSamples: 0 };
      if (command === "meeting_capture_status") return { meetingId: record.id, active: true, paused: true, level: 0, recordedSamples: capped.recordedSamples };
      if (command === "meeting_get") return finished ? { ...capped, status: "recorded" } : capped;
      if (command === "meeting_capture_stop") { finished = true; return { ...capped, status: "recorded" }; }
    });
    await meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    await vi.waitFor(() => expect(meetingRecorder.current()).toMatchObject({
      active: false, pendingSave: false, meeting: { recordedSamples: MAX_MEETING_RECORDING_SAMPLES, status: "recorded" },
    }), { timeout: 1500 });
    expect(mocks.invoke.mock.calls.filter(([command]) => command === "meeting_capture_stop")).toHaveLength(1);
  });

  it("退出先等待连接检查结束并取消启动，退出失败后可重新开始录音", async () => {
    let release!: () => void;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_check_connection") { await pending; return; }
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") return { meetingId: record.id, active: true };
      if (command === "meeting_capture_stop") return { ...record, status: "recorded" };
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    const { saveRecordingBeforeExit } = await import("../recording-exit");
    const started = meetingRecorder.start(record.title, record.modelId, record.providerId, "both").catch((error) => error);
    let exitFinished = false;
    const exit = saveRecordingBeforeExit().then(() => { exitFinished = true; });
    await Promise.resolve();
    expect(exitFinished).toBe(false);
    await expect(meetingRecorder.start(record.title, record.modelId, record.providerId, "both")).rejects.toThrow("正在准备退出");
    release();
    expect(await started).toBeInstanceOf(Error);
    await exit;
    expect(meetingRecorder.current().active).toBe(false);
    expect(mocks.invoke.mock.calls.some(([command]) => command === "meeting_capture_start")).toBe(false);
    meetingRecorder.resumeAfterExitFailure();
    await meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    await meetingRecorder.stop();
  });

  it("原生采集已发起时，退出等待其启动结果并保存同一录音一次", async () => {
    let release!: () => void;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") { await pending; return { meetingId: record.id, active: true, paused: false }; }
      if (command === "meeting_capture_stop") return { ...record, status: "recorded", recordedSamples: 160 };
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    const { saveRecordingBeforeExit } = await import("../recording-exit");
    const started = meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith("meeting_capture_start", expect.anything()));
    const exit = saveRecordingBeforeExit();
    release();
    await Promise.all([started, exit]);
    expect(meetingRecorder.current()).toMatchObject({ active: false, pendingSave: false });
    expect(mocks.invoke.mock.calls.filter(([command]) => command === "meeting_capture_stop")).toHaveLength(1);
  });

  it("麦克风授权迟到时释放设备，退出请求结束后也不复活已取消的启动", async () => {
    let release!: (stream: MediaStream) => void;
    const stop = vi.fn();
    Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: {
      getUserMedia: vi.fn(() => new Promise<MediaStream>((resolve) => { release = resolve; })),
    } });
    const { meetingRecorder } = await import("../meeting-minutes");
    const { saveRecordingBeforeExit } = await import("../recording-exit");
    try {
      const started = meetingRecorder.start(record.title, record.modelId, record.providerId).catch((error) => error);
      await vi.waitFor(() => expect(release).toBeDefined());
      const exit = saveRecordingBeforeExit();
      meetingRecorder.resumeAfterExitFailure();
      release({ getTracks: () => [{ stop }] } as unknown as MediaStream);
      expect(await started).toBeInstanceOf(Error);
      await exit;
      expect(stop).toHaveBeenCalledOnce();
      expect(meetingRecorder.current().active).toBe(false);
      expect(mocks.invoke.mock.calls.some(([command]) => command === "meeting_create")).toBe(false);
    } finally {
      Reflect.deleteProperty(navigator, "mediaDevices");
    }
  });

  it("waits for an in-flight pause before exit finalizes the recording", async () => {
    let release!: () => void;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    let pausedCompleted = false;
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") return { meetingId: record.id, active: true, paused: false, level: 0, recordedSamples: 160 };
      if (command === "meeting_capture_pause") { await pending; pausedCompleted = true; return { ...record, status: "paused" }; }
      if (command === "meeting_capture_stop") { expect(pausedCompleted).toBe(true); return { ...record, status: "recorded", recordedSamples: 160 }; }
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    await meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    const pause = meetingRecorder.setPaused(true);
    const exit = meetingRecorder.stop();
    try {
      await Promise.resolve();
      expect(mocks.invoke.mock.calls.filter(([command]) => command === "meeting_capture_stop")).toHaveLength(0);
      await expect(meetingRecorder.setPaused(false)).rejects.toThrow("正在结束录音");
    } finally {
      release();
      await Promise.all([pause, exit]);
    }
    expect(meetingRecorder.current().active).toBe(false);
  });

  it("shares one finalization when the toolbar and exit both stop the recording", async () => {
    let release!: () => void;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") return { meetingId: record.id, active: true, paused: false, level: 0, recordedSamples: 160 };
      if (command === "meeting_capture_stop") { await pending; return { ...record, status: "recorded", recordedSamples: 160 }; }
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    await meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    const toolbar = meetingRecorder.stop();
    const exit = meetingRecorder.stop();
    try {
      expect(toolbar).toBe(exit);
      await vi.waitFor(() => expect(mocks.invoke.mock.calls.filter(([command]) => command === "meeting_capture_stop")).toHaveLength(1));
    } finally {
      release();
      await Promise.all([toolbar, exit]);
    }
    expect(meetingRecorder.current().active).toBe(false);
  });

  it("keeps a failed native save reachable and retries the same recording", async () => {
    let attempts = 0;
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") return { meetingId: record.id, active: true, paused: false, level: 0, recordedSamples: 160 };
      if (command === "meeting_capture_stop") {
        if (++attempts === 1) throw new Error("磁盘已满");
        return { ...record, status: "recorded", recordedSamples: 160 };
      }
      if (command === "meeting_capture_status") return { meetingId: record.id, active: false, paused: true, pendingSave: true, level: 0, recordedSamples: 160, error: "磁盘已满" };
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    await meetingRecorder.start(record.title, record.modelId, record.providerId, "both");
    await expect(meetingRecorder.stop()).rejects.toThrow("磁盘已满");
    expect(meetingRecorder.current()).toMatchObject({ active: true, paused: true, pendingSave: true, source: "both" });
    await expect(meetingRecorder.start(record.title, record.modelId, record.providerId, "both")).rejects.toThrow("已有会议");
    await meetingRecorder.stop();
    expect(meetingRecorder.current()).toMatchObject({ active: false, pendingSave: false, error: null });
  });

  it("removes an empty record if native capture cannot start", async () => {
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return record;
      if (command === "meeting_capture_start") throw new Error("未授权系统音频录制");
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    await expect(meetingRecorder.start(record.title, record.modelId, record.providerId, "both")).rejects.toThrow("未授权系统音频录制");
    expect(mocks.invoke).toHaveBeenCalledWith("meeting_delete", { meetingId: record.id });
    expect(meetingRecorder.current().active).toBe(false);
  });
});


describe("microphone save recovery", () => {
  it("pauses on a failed periodic write, bounds audio buffering and retries before resuming", async () => {
    vi.resetModules();
    mocks.invoke.mockReset();
    let processor: { onaudioprocess?: (event: unknown) => void } = {};
    const source = { channelCount: 1, connect: vi.fn(), disconnect: vi.fn() };
    vi.stubGlobal("AudioContext", vi.fn(function () { return {
      createMediaStreamSource: () => source,
      createScriptProcessor: () => (processor = { connect: vi.fn(), disconnect: vi.fn() } as typeof processor),
      destination: {}, close: vi.fn().mockResolvedValue(undefined),
    }; }));
    Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: {
      getUserMedia: vi.fn().mockResolvedValue({ getTracks: () => [{ stop: vi.fn() }] }),
    } });
    let diskFull = true;
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return { ...record, captureSource: "microphone" };
      if (command === "meeting_append_pcm") { if (diskFull) throw new Error("磁盘已满"); return { ...record, recordedSamples: 80000 }; }
      if (command === "meeting_set_paused") return { ...record, status: "recording", recordedSamples: 80000 };
      if (command === "meeting_finish_recording") return { ...record, status: "recorded", recordedSamples: 80000 };
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    const event = { inputBuffer: { numberOfChannels: 1, length: 80000, sampleRate: 16000, getChannelData: () => new Float32Array(80000).fill(0.2) } };
    try {
      await meetingRecorder.start(record.title, record.modelId, record.providerId);
      processor.onaudioprocess?.(event);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(meetingRecorder.current()).toMatchObject({ active: true, paused: true, error: "磁盘已满", pendingSave: false });
      processor.onaudioprocess?.(event);
      diskFull = false;
      await meetingRecorder.setPaused(false);
      expect(meetingRecorder.current()).toMatchObject({ active: true, paused: false, error: null });
      await meetingRecorder.stop();
      const writes = mocks.invoke.mock.calls.filter(([command]) => command === "meeting_append_pcm");
      expect(writes).toHaveLength(2);
      expect(writes.every(([, payload]) => payload.samples.length === 80000)).toBe(true);
    } finally {
      diskFull = false;
      if (meetingRecorder.current().active) await meetingRecorder.stop().catch(() => undefined);
      vi.unstubAllGlobals();
      Reflect.deleteProperty(navigator, "mediaDevices");
    }
  });

  it("stops accepting microphone PCM immediately while pause waits for a disk write", async () => {
    vi.resetModules();
    mocks.invoke.mockReset();
    let processor: { onaudioprocess?: (event: unknown) => void } = {};
    const source = { channelCount: 1, connect: vi.fn(), disconnect: vi.fn() };
    vi.stubGlobal("AudioContext", vi.fn(function () { return {
      createMediaStreamSource: () => source,
      createScriptProcessor: () => (processor = { connect: vi.fn(), disconnect: vi.fn() } as typeof processor),
      destination: {}, close: vi.fn().mockResolvedValue(undefined),
    }; }));
    Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: {
      getUserMedia: vi.fn().mockResolvedValue({ getTracks: () => [{ stop: vi.fn() }] }),
    } });
    let releaseWrite!: () => void;
    const pendingWrite = new Promise<void>((resolve) => { releaseWrite = resolve; });
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return { ...record, captureSource: "microphone" };
      if (command === "meeting_append_pcm") { await pendingWrite; return { ...record, recordedSamples: 160 }; }
      if (command === "meeting_set_paused") return { ...record, status: "paused", recordedSamples: 160 };
      if (command === "meeting_finish_recording") return { ...record, status: "recorded", recordedSamples: 160 };
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    const event = { inputBuffer: { numberOfChannels: 1, length: 160, sampleRate: 16000, getChannelData: () => new Float32Array(160).fill(0.2) } };
    try {
      await meetingRecorder.start(record.title, record.modelId, record.providerId);
      processor.onaudioprocess?.(event);
      const pausing = meetingRecorder.setPaused(true);
      expect(meetingRecorder.current().paused).toBe(true);
      processor.onaudioprocess?.(event);
      releaseWrite();
      await pausing;
      await meetingRecorder.stop();
      const appends = mocks.invoke.mock.calls.filter(([command]) => command === "meeting_append_pcm");
      expect(appends).toHaveLength(1);
      expect(appends[0][1].samples).toHaveLength(160);
    } finally {
      releaseWrite();
      vi.unstubAllGlobals();
      Reflect.deleteProperty(navigator, "mediaDevices");
    }
  });

  it("keeps buffered audio available for retry and refuses a new recording until saved", async () => {
    vi.resetModules();
    mocks.invoke.mockReset();
    let processor: { onaudioprocess?: (event: unknown) => void } = {};
    const stopTrack = vi.fn();
    const disconnect = vi.fn();
    const source = { channelCount: 1, connect: vi.fn(), disconnect };
    const audioContext = {
      createMediaStreamSource: () => source,
      createScriptProcessor: () => (processor = { connect: vi.fn(), disconnect } as typeof processor),
      destination: {}, close: vi.fn().mockResolvedValue(undefined),
    };
    vi.stubGlobal("AudioContext", vi.fn(function () { return audioContext; }));
    Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: {
      getUserMedia: vi.fn().mockResolvedValue({ getTracks: () => [{ stop: stopTrack }] }),
    } });
    let diskFull = true;
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "meeting_create") return { ...record, captureSource: "microphone" };
      if (command === "meeting_append_pcm") {
        if (diskFull) throw new Error("磁盘已满");
        return { ...record, recordedSamples: 160, durationSeconds: 0.01 };
      }
      if (command === "meeting_finish_recording") return { ...record, status: "recorded", recordedSamples: 160 };
      return undefined;
    });
    const { meetingRecorder } = await import("../meeting-minutes");
    try {
      await meetingRecorder.start(record.title, record.modelId, record.providerId);
      processor.onaudioprocess?.({ inputBuffer: {
        numberOfChannels: 1, length: 160, sampleRate: 16000,
        getChannelData: () => new Float32Array(160).fill(0.2),
      } });
      await expect(meetingRecorder.stop()).rejects.toThrow("磁盘已满");
      expect(stopTrack).toHaveBeenCalledOnce();
      expect(meetingRecorder.current()).toMatchObject({ active: true, paused: true, pendingSave: true, source: "microphone" });
      await expect(meetingRecorder.start(record.title, record.modelId, record.providerId)).rejects.toThrow("已有会议");
      await expect(meetingRecorder.setPaused(false)).rejects.toThrow("重试保存");
      diskFull = false;
      await meetingRecorder.stop();
      expect(meetingRecorder.current()).toMatchObject({ active: false, pendingSave: false, error: null });
      expect(mocks.invoke).toHaveBeenCalledWith("meeting_append_pcm", { meetingId: record.id, samples: expect.arrayContaining([6553]) });
    } finally {
      vi.unstubAllGlobals();
      Reflect.deleteProperty(navigator, "mediaDevices");
    }
  });
});
