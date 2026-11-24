import { beforeEach, expect, it, vi } from "vitest";
vi.mock("../meeting-minutes", () => ({ meetingRecorder: { current: vi.fn(), stop: vi.fn(), prepareForExit: vi.fn(), resumeAfterExitFailure: vi.fn() } }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
import { invoke } from "@tauri-apps/api/core";
import { meetingRecorder } from "../meeting-minutes";
import { createExitPreparationController, saveRecordingBeforeExit } from "../recording-exit";
beforeEach(() => vi.resetAllMocks());
it("无录音时正常退出，活动录音先完成保存", async () => {
  vi.mocked(meetingRecorder.current).mockReturnValue({ active: false, pendingSave: false } as never);
  await saveRecordingBeforeExit();
  expect(meetingRecorder.stop).not.toHaveBeenCalled();
  vi.mocked(meetingRecorder.current).mockReturnValue({ active: true } as never);
  vi.mocked(meetingRecorder.stop).mockResolvedValue({} as never);
  await saveRecordingBeforeExit();
  expect(meetingRecorder.stop).toHaveBeenCalledTimes(1);
});
it("保存失败仍有缓冲时拒绝退出，并允许同一录音重试", async () => {
  vi.mocked(meetingRecorder.current).mockReturnValue({ active: true, pendingSave: true } as never);
  vi.mocked(meetingRecorder.stop).mockRejectedValueOnce(new Error("disk full")).mockResolvedValueOnce({} as never);
  await expect(saveRecordingBeforeExit()).rejects.toThrow("disk full");
  await expect(saveRecordingBeforeExit()).resolves.toBeUndefined();
  expect(meetingRecorder.stop).toHaveBeenCalledTimes(2);
});
it("空录音已安全移除后不永久阻止退出", async () => {
  vi.mocked(meetingRecorder.current).mockReturnValueOnce({ active: true } as never).mockReturnValue({ active: false, pendingSave: false } as never);
  vi.mocked(meetingRecorder.stop).mockRejectedValue(new Error("未录到音频，空记录已移除"));
  await expect(saveRecordingBeforeExit()).resolves.toBeUndefined();
});
it("native录音异常结束仍有会议记录时不能忽略保存失败", async () => {
  vi.mocked(meetingRecorder.current).mockReturnValueOnce({ active: true } as never)
    .mockReturnValue({ active: false, pendingSave: false, meeting: { id: "unfinalized" } } as never);
  vi.mocked(meetingRecorder.stop).mockRejectedValue(new Error("录音任务异常结束"));
  await expect(saveRecordingBeforeExit()).rejects.toThrow("录音任务异常结束");
});
it("超时请求的迟到保存和错误不影响下一次退出请求", async () => {
  let resolveFirst!: (value: never) => void;
  let resolveSecond!: (value: never) => void;
  vi.mocked(meetingRecorder.current).mockReturnValue({ active: true } as never);
  vi.mocked(meetingRecorder.stop).mockReturnValueOnce(new Promise((done) => { resolveFirst = done; }))
    .mockReturnValueOnce(new Promise((done) => { resolveSecond = done; }));
  vi.mocked(invoke).mockResolvedValue(true);
  const preparing = vi.fn();
  const failed = vi.fn();
  const exit = createExitPreparationController({ preparing, failed });
  exit.prepare({ requestId: "old" });
  await vi.waitFor(() => expect(meetingRecorder.stop).toHaveBeenCalledTimes(1));
  exit.fail({ requestId: "old", message: "timeout" });
  exit.prepare({ requestId: "new" });
  await vi.waitFor(() => expect(meetingRecorder.stop).toHaveBeenCalledTimes(2));
  resolveFirst({} as never);
  await Promise.resolve();
  await Promise.resolve();
  expect(invoke).not.toHaveBeenCalled();
  exit.fail({ requestId: "old", message: "late failure" });
  expect(preparing).toHaveBeenLastCalledWith("recording");
  exit.prepare({ requestId: "old" }, true);
  resolveSecond({} as never);
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("desktop_exit_prepared", { requestId: "new", error: null }));
  expect(preparing).toHaveBeenLastCalledWith("tasks");
});
it("已保存阶段的重挂载快照不重复停止录音", () => {
  const preparing = vi.fn();
  const exit = createExitPreparationController({ preparing, failed: vi.fn() });
  exit.prepare({ requestId: "prepared", recordingSaved: true }, true);
  expect(preparing).toHaveBeenCalledWith("tasks");
  expect(meetingRecorder.stop).not.toHaveBeenCalled();
});
