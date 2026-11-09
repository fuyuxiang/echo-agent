import { meetingDrafts } from "./meeting-drafts";
import { meetingRecorder } from "./meeting-minutes";
import { invoke } from "@tauri-apps/api/core";

export interface DesktopExitNotice { requestId: string; recordingSaved?: boolean }
export interface DesktopExitFailure { requestId?: string | null; message: string }

/** A stopped empty recording has no buffered data to lose. A failed save does. */
export async function saveRecordingBeforeExit(): Promise<void> {
  meetingDrafts.ensureDurable();
  await meetingRecorder.prepareForExit();
  const snapshot = meetingRecorder.current();
  if (!snapshot.active && !snapshot.pendingSave) return;
  try {
    await meetingRecorder.stop();
  } catch (error) {
    const remaining = meetingRecorder.current();
    if (remaining.active || remaining.pendingSave || remaining.meeting) throw error;
  }
}

/** Request-owned handlers shared by the actual shell and its IPC regression. */
export function createExitPreparationController(callbacks: {
  preparing: (preparing: boolean) => void;
  failed: (message: string) => void;
  beforePrepare?: (isCurrent: () => boolean) => Promise<void>;
  cancelPrepare?: () => void;
}) {
  let disposed = false;
  let active: string | null = null;
  const closed = new Set<string>();
  const rememberClosed = (id: string) => {
    closed.add(id);
    if (closed.size > 64) closed.delete(closed.values().next().value!);
  };
  const fail = (notice: DesktopExitFailure) => {
    if (disposed) return;
    if (notice.requestId) {
      rememberClosed(notice.requestId);
      if (notice.requestId !== active) return;
    }
    if (active) rememberClosed(active);
    active = null;
    callbacks.cancelPrepare?.();
    meetingDrafts.cancelExitDiscard();
    meetingRecorder.resumeAfterExitFailure();
    callbacks.preparing(false);
    callbacks.failed(notice.message);
  };
  const acknowledge = async (id: string, error: string | null) => {
    if (disposed || active !== id || closed.has(id)) return;
    const accepted = await invoke<boolean>("desktop_exit_prepared", { requestId: id, error });
    if (!disposed && active === id && accepted === false) {
      fail({ requestId: id, message: "退出准备请求已结束，应用已保留，请重试退出。" });
    }
  };
  const prepare = (notice: DesktopExitNotice, replay = false) => {
    if (disposed || !notice?.requestId || closed.has(notice.requestId)) return;
    // A listener-registration snapshot can arrive after a newer live request.
    if (replay && active && active !== notice.requestId) return;
    if (active === notice.requestId) return;
    active = notice.requestId;
    callbacks.preparing(true);
    if (notice.recordingSaved) {
      void meetingRecorder.prepareForExit();
      return;
    }
    const id = notice.requestId;
    const isCurrent = () => !disposed && active === id && !closed.has(id);
    const save = callbacks.beforePrepare
      ? Promise.resolve().then(() => callbacks.beforePrepare?.(isCurrent)).then(() => isCurrent() ? saveRecordingBeforeExit() : undefined)
      : saveRecordingBeforeExit();
    void save.then(
      () => acknowledge(id, null),
      (error) => acknowledge(id, String(error).replace(/^Error:\s*/, "").slice(0, 1024)),
    ).catch((error) => {
      if (!disposed && active === id) fail({ requestId: id, message: `无法确认录音保存：${String(error).replace(/^Error:\s*/, "")}` });
    });
  };
  return {
    prepare,
    fail,
    dispose: () => { disposed = true; active = null; meetingDrafts.cancelExitDiscard(); meetingRecorder.resumeAfterExitFailure(); },
  };
}
