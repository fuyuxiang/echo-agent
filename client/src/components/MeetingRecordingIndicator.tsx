import { useEffect, useState } from "react";
import { CircleStop, Mic, MonitorPlay, Pause, Play } from "lucide-react";
import { formatMeetingDuration, meetingRecorder } from "@/lib/meeting-minutes";

export function MeetingRecordingIndicator({
  onOpen,
  onToast,
}: {
  onOpen: () => void;
  onToast?: (message: string) => void;
}) {
  const [snapshot, setSnapshot] = useState(meetingRecorder.current());
  const [pending, setPending] = useState(false);
  useEffect(() => {
    const unsubscribe = meetingRecorder.subscribe(setSnapshot);
    void meetingRecorder.syncNative().catch(() => undefined);
    return unsubscribe;
  }, []);
  if (!snapshot.active || !snapshot.meeting) return null;

  const toggle = async () => {
    setPending(true);
    try {
      await meetingRecorder.setPaused(!snapshot.paused);
    } catch (error) {
      onToast?.(`切换录音状态失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setPending(false);
    }
  };
  const stop = async () => {
    setPending(true);
    try {
      const record = await meetingRecorder.stop();
      onOpen();
      onToast?.(record.autoMinutesTarget ? "录音已保存，将按本次设置生成纪要" : "录音已保存，原文在本机转写");
    } catch (error) {
      onToast?.(`结束录音失败：${String(error).replace(/^Error:\s*/, "")}`);
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="meeting-global-recorder" role="status" aria-live="polite">
      <button className="meeting-global-recorder__main" type="button" onClick={onOpen} title="打开录音转写工作台">
        <span className="meeting-global-recorder__dot">{snapshot.source === "microphone" ? <Mic size={14} /> : <MonitorPlay size={14} />}</span>
        <span><strong>{snapshot.error ? "录音需要处理" : snapshot.paused ? "录音已暂停" : "正在录音"}</strong><small>{snapshot.source === "both" ? "双方声音" : snapshot.source === "system" ? "系统声音" : "麦克风"} · {formatMeetingDuration(snapshot.meeting.durationSeconds)}</small></span>
      </button>
      <button type="button" onClick={() => void toggle()} disabled={pending || Boolean(snapshot.error)} aria-label={snapshot.paused ? "继续录音" : "暂停录音"}>{snapshot.paused ? <Play size={14} /> : <Pause size={14} />}</button>
      <button type="button" onClick={() => void stop()} disabled={pending} aria-label="结束并保存录音"><CircleStop size={14} /></button>
    </div>
  );
}
