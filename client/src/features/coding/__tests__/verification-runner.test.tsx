import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CodingTask, DetectedCommand, VerificationRecord } from "../lib/types";

const api = vi.hoisted(() => ({
  beginVerification: vi.fn(), runVerification: vi.fn(), reportVerification: vi.fn(),
  reportStartFailed: vi.fn(), approvePlanCommand: vi.fn(),
}));
vi.mock("../lib/tauri-api", () => ({ codingApi: api }));

import { useVerificationRunner } from "../lib/use-verification-runner";
import { codingRuntimeKey, useCodingRuntimeStore } from "../lib/coding-task-runtime";
import { useTaskStore } from "../store/task-store";

function task(id: string): CodingTask {
  return {
    schemaVersion: 2, id, name: id, requirement: "验证", phase: "stopped",
    acceptanceCriteria: [], taskNodes: [], planIssues: [], globalConstraints: [], createdAt: "", updatedAt: "1",
  };
}
const originalRefreshTaskState = useTaskStore.getState().refreshTaskState;
const commands: DetectedCommand[] = [{ kind: "test", command: "pnpm test", label: "测试" }];

describe("verification runner task scope", () => {
  beforeEach(() => {
    Object.values(api).forEach((mock) => mock.mockReset());
    api.beginVerification.mockImplementation(async (_root: string, id: string) => task(id));
    api.reportVerification.mockResolvedValue(undefined);
    api.reportStartFailed.mockResolvedValue(undefined);
    useCodingRuntimeStore.setState({ tasks: {}, settling: {}, activeRunIds: {}, verificationBatches: {}, stopping: {}, actionEpochs: {}, documentCancellationReady:true,documentCancellations:{} });
    useTaskStore.setState({ root: "", task: null, verifications: [], refreshTaskState: originalRefreshTaskState });
  });

  it("立即重复点击仍只启动一个验证批次", async () => {
    let resolve!: (value: { status: string }) => void;
    api.runVerification.mockReturnValue(new Promise((accept) => { resolve = accept; }));
    const { result } = renderHook(() => useVerificationRunner({
      cwd: "/a", task: task("t1"), commands, detectedReady: true,
      setCommandOutput: vi.fn(), requestConfirmation: vi.fn(),
    }));
    act(() => {
      result.current.runVerifications(commands);
      result.current.runVerifications(commands);
    });
    await waitFor(() => expect(api.runVerification).toHaveBeenCalledTimes(1));
    expect(api.beginVerification).toHaveBeenCalledTimes(1);
    await act(async () => { resolve({ status: "passed" }); });
    await waitFor(() => expect(result.current.runningVerification).toBe(false));
  });

  it("切项目后原批次保持可取消且旧失败不会清掉新批次状态", async () => {
    let failOld!: (reason: Error) => void;
    let finishNew!: (value: { status: string }) => void;
    api.runVerification.mockImplementation((root: string) => root === "/a"
      ? new Promise((_accept, reject) => { failOld = reject; })
      : new Promise((accept) => { finishNew = accept; }));
    const onToast = vi.fn();
    const setCommandOutput = vi.fn();
    const { result, rerender } = renderHook(({ root, id }) => useVerificationRunner({
      cwd: root, task: task(id), commands, detectedReady: true,
      setCommandOutput, requestConfirmation: vi.fn(), onToast,
    }), { initialProps: { root: "/a", id: "t1" } });
    act(() => { result.current.runVerifications(commands); });
    await waitFor(() => expect(api.runVerification).toHaveBeenCalledTimes(1));
    const oldRun = useCodingRuntimeStore.getState().activeRunIds[codingRuntimeKey("/a", "t1")];
    expect(oldRun).toBeTruthy();
    rerender({ root: "/b", id: "t2" });
    expect(result.current.runningVerification).toBe(false);
    expect(result.current.activeRunId).toBeNull();
    expect(useCodingRuntimeStore.getState().activeRunIds[codingRuntimeKey("/a", "t1")]).toBe(oldRun);
    act(() => { result.current.runVerifications(commands); });
    await waitFor(() => expect(api.runVerification).toHaveBeenCalledTimes(2));
    const newRun = result.current.activeRunId;
    expect(newRun).toBeTruthy();
    await act(async () => { failOld(new Error("旧项目验证失败")); });
    expect(result.current.runningVerification).toBe(true);
    expect(result.current.activeRunId).toBe(newRun);
    expect(onToast).not.toHaveBeenCalled();
    expect(api.reportStartFailed).toHaveBeenCalledWith("/a", "t1", expect.stringContaining("旧项目验证失败"));
    await act(async () => { finishNew({ status: "passed" }); });
    await waitFor(() => expect(result.current.runningVerification).toBe(false));
  });
  it("验证保存导致自身暂停后，使用保存完成的修订号进行准入", async () => {
    api.runVerification.mockResolvedValue({status:"passed"});
    const {result} = renderHook(() => useVerificationRunner({cwd:"/a",task:task("t1"),commands,detectedReady:true,
      setCommandOutput:vi.fn(),requestConfirmation:vi.fn(),prepareEditor:async () => "saved-revision"}));
    act(() => result.current.runVerifications(commands));
    await waitFor(() => expect(api.beginVerification).toHaveBeenCalledWith("/a", "t1", "saved-revision"));
    await waitFor(() => expect(result.current.runningVerification).toBe(false));
  });

  it("停止后迟到的编辑器确认不会重新启动验证", async () => {
    let ready!: () => void;
    const prepareEditor = vi.fn(() => new Promise<void>(resolve => {ready = resolve;}));
    const {result} = renderHook(() => useVerificationRunner({
      cwd: "/a", task: task("t1"), commands, detectedReady: true,
      setCommandOutput: vi.fn(), requestConfirmation: vi.fn(), prepareEditor,
    }));
    act(() => {result.current.runVerifications(commands);});
    await waitFor(() => expect(prepareEditor).toHaveBeenCalledOnce());
    await act(async () => {
      useCodingRuntimeStore.getState().setStopping("/a", "t1", true);
      ready();
    });
    expect(api.beginVerification).not.toHaveBeenCalled();
    expect(api.runVerification).not.toHaveBeenCalled();
    expect(result.current.runningVerification).toBe(false);
  });

  it("停止使旧批次失效，迟到的命令失败不能改写停止结果", async () => {
    let reject!: (error: Error) => void;
    api.runVerification.mockReturnValue(new Promise((_resolve, fail) => {reject = fail;}));
    const onToast = vi.fn();
    const {result} = renderHook(() => useVerificationRunner({
      cwd: "/a", task: task("t1"), commands, detectedReady: true,
      setCommandOutput: vi.fn(), requestConfirmation: vi.fn(), onToast,
    }));
    act(() => {result.current.runVerifications(commands);});
    await waitFor(() => expect(api.runVerification).toHaveBeenCalledOnce());
    expect(api.beginVerification).toHaveBeenCalledWith("/a", "t1", "1");
    await act(async () => {
      useCodingRuntimeStore.getState().setStopping("/a", "t1", true);
      useCodingRuntimeStore.getState().setStopping("/a", "t1", false);
      reject(new Error("late failure"));
    });
    expect(api.reportStartFailed).not.toHaveBeenCalled();
    expect(api.reportVerification).not.toHaveBeenCalled();
    expect(onToast).not.toHaveBeenCalled();
    expect(result.current.runningVerification).toBe(false);
  });

  it("runs only the selected command and preserves another check's same-revision result", async () => {
    const second = { kind: "build" as const, command: "pnpm build", label: "构建" };
    const both = [...commands, second];
    useTaskStore.setState({ root: "/a", task: task("t1"), verifications: [], refreshTaskState: vi.fn().mockResolvedValue(undefined) });
    api.runVerification.mockImplementation(async (_root: string, taskId: string, kind: VerificationRecord["kind"], command: string) => {
      const record: VerificationRecord = { id: command, taskId, kind, command, status: "passed", exitCode: 0,
        stdout: "", stderr: "", durationMs: 1, startedAt: "", finishedAt: "", structured: false,
        contentRevision: "same-source", configurationRevision: "same-config" };
      useTaskStore.getState().applyVerification(record);
      return record;
    });
    const { result } = renderHook(() => useVerificationRunner({ cwd: "/a", task: task("t1"), commands: both,
      detectedReady: true, setCommandOutput: vi.fn(), requestConfirmation: vi.fn() }));
    act(() => result.current.runVerifications([both[0]]));
    await waitFor(() => expect(api.reportVerification).toHaveBeenCalledTimes(1));
    expect(api.runVerification).toHaveBeenCalledTimes(1);
    expect(useTaskStore.getState().verifications.map(record => record.command)).toEqual(["pnpm test"]);
    act(() => result.current.runVerifications([both[1]]));
    await waitFor(() => expect(api.reportVerification).toHaveBeenCalledTimes(2));
    expect(useTaskStore.getState().verifications.map(record => record.command)).toEqual(["pnpm test", "pnpm build"]);
    expect(api.reportStartFailed).not.toHaveBeenCalled();
    expect(api.runVerification.mock.calls.map(call => call[3])).toEqual(["pnpm test", "pnpm build"]);
  });

});
