import { modelChoiceLabel } from "@/lib/model-presentation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  CircleStop,
  Clock3,
  Copy,
  Download,
  FileAudio,
  FileText,
  Headphones,
  Loader2,
  Mic,
  MonitorPlay,
  Pause,
  Play,
  RefreshCw,
  Sparkles,
  Trash2,
  Upload,
} from "lucide-react";
import { copyShareText } from "@/lib/share";
import { meetingDrafts, type MeetingDraftConflict } from "@/lib/meeting-drafts";
import { filesystemPickFiles } from "@/lib/agent-client";
import {
  formatMeetingDuration,
  meetingCaptureSupport,
  meetingDelete,
  isMiniMaxMeetingModel,
  meetingExport,
  meetingGet,
  meetingImportAudio,
  meetingJobActive,
  meetingList,
  meetingOpenAudio,
  meetingProcess,
  meetingRecorder,
  meetingRegenerateMinutes,
  meetingUpdate,
  type MeetingRecord,
  type RecordingSource,
  type CaptureSupport,
  type TranscriptSegment,
} from "@/lib/meeting-minutes";
import { useMeetingTranscriptionCapability } from "@/lib/use-meeting-transcription-capability";
import type { ModelOption } from "./ModelSelector";
import { Markdown } from "./markdown/Markdown";

interface MeetingMinutesPanelProps {
  modelId?: string;
  models: ModelOption[];
  onToast?: (message: string) => void;
  onOpenModelSettings?: () => void;
}

type DetailTab = "minutes" | "transcript" | "audio";

const STATUS_LABELS: Record<string, string> = {
  recording: "录音中",
  paused: "已暂停",
  interrupted: "录音已中断",
  importing: "正在导入",
  recorded: "等待转写",
  transcribing: "正在转写",
  summarizing: "正在生成纪要",
  transcribed: "转写已完成",
  partial: "部分转写失败",
  failed: "处理失败",
  completed: "已完成",
};

const NATIVE_JOB_STATUSES = ["importing", "transcribing", "summarizing"];

const SOURCE_LABELS: Record<RecordingSource, string> = {
  microphone: "麦克风",
  system: "系统声音",
  both: "麦克风 + 系统声音",
};
const SOURCE_SHORT_LABELS: Record<RecordingSource, string> = {
  microphone: "麦克风",
  system: "系统音",
  both: "混合录音",
};

function errorText(error: unknown): string {
  return String(error).replace(/^Error:\s*/, "");
}

function defaultMeetingTitle(): string {
  return `会议 ${new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date())}`;
}

export function MeetingMinutesPanel({
  modelId,
  models,
  onToast,
  onOpenModelSettings,
}: MeetingMinutesPanelProps) {
  const [chosenModelId, setChosenModelId] = useState<string | null>(null);
  const [selected, setSelected] = useState<MeetingRecord | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const meetingModels = models.filter((model) => model.providerId
    && model.source !== "builtin"
    && model.providerId !== "echoagent-ojlab"
    && !model.id.startsWith("echoagent-ojlab/")
    && isMiniMaxMeetingModel(model));
  const selectedModel = meetingModels.find((model) => model.id === chosenModelId)
    ?? (!showCreate ? meetingModels.find((model) => model.id === selected?.modelId && model.providerId === selected.providerId) : undefined)
    ?? meetingModels.find((model) => model.id === modelId)
    ?? meetingModels[0];
  const connection = useMeetingTranscriptionCapability(selectedModel, models);
  const capable = connection.available;
  const [records, setRecords] = useState<MeetingRecord[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selectedId;
  const [transcriptConflict, setTranscriptConflict] = useState<MeetingDraftConflict | undefined>();
  const [minutesConflict, setMinutesConflict] = useState<MeetingDraftConflict | undefined>();
  const [hasMinutesDraft, setHasMinutesDraft] = useState(false);
  const [hasTranscriptDraft, setHasTranscriptDraft] = useState(false);
  const [editingMinutes, setEditingMinutes] = useState(false);
  const [draftMinutes, setDraftMinutes] = useState("");
  const [title, setTitle] = useState(defaultMeetingTitle);
  const [tab, setTab] = useState<DetailTab>("minutes");
  const [busy, setBusy] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [, setDraftRevision] = useState(0);
  const [draftTranscript, setDraftTranscript] = useState<TranscriptSegment[]>([]);
  const [recorder, setRecorder] = useState(meetingRecorder.current());
  const [activeJobs, setActiveJobs] = useState<Record<string, boolean | undefined>>({});
  const [recordingSource, setRecordingSource] = useState<RecordingSource>("microphone");
  const [captureSupport, setCaptureSupport] = useState<CaptureSupport | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);

  const applyRecord = useCallback((record: MeetingRecord) => {
    const transcript = meetingDrafts.transcript(record);
    const minutes = meetingDrafts.minutes(record);
    setSelected(record);
    setDraftTranscript(transcript.value);
    setTranscriptConflict(transcript.conflict);
    setDraftMinutes(minutes.value);
    setMinutesConflict(minutes.conflict);
    setHasMinutesDraft(meetingDrafts.hasMinutes(record.id));
    setHasTranscriptDraft(meetingDrafts.hasTranscript(record.id));
    if (minutes.conflict) setEditingMinutes(false);
  }, []);

  useEffect(() => {
    void meetingCaptureSupport().then(setCaptureSupport).catch(() => setCaptureSupport({ systemAudio: false, detail: "无法检测系统声音采集能力" }));
    void meetingRecorder.syncNative().catch(() => undefined);
  }, []);

  const refreshJobState = useCallback(async (record: MeetingRecord) => {
    if (!NATIVE_JOB_STATUSES.includes(record.status)) {
      setActiveJobs((current) => current[record.id] === false ? current : { ...current, [record.id]: false });
      return;
    }
    setActiveJobs((current) => ({ ...current, [record.id]: undefined }));
    try {
      const active = await meetingJobActive(record.id);
      setActiveJobs((current) => ({ ...current, [record.id]: active }));
    } catch {
      setActiveJobs((current) => current[record.id] === false ? current : { ...current, [record.id]: false });
    }
  }, []);

  const refreshList = useCallback(async (preferId?: string | null, expectedSelectedId?: string) => {
    try {
      const next = await meetingList();
      setRecords(next);
      setLoadError(null);
      if (expectedSelectedId !== undefined && selectedIdRef.current !== expectedSelectedId) return;
      const target = preferId === null ? next[0]?.id : preferId ?? selectedIdRef.current ?? next[0]?.id;
      if (target) {
        const record = next.find((item) => item.id === target) ?? await meetingGet(target);
        if (expectedSelectedId !== undefined && selectedIdRef.current !== expectedSelectedId) return;
        selectedIdRef.current = record.id;
        setSelectedId(record.id);
        applyRecord(record);
        void refreshJobState(record);
      } else {
        setSelectedId(null);
        setSelected(null);
        setDraftTranscript([]);
      }
    } catch (error) {
      setLoadError(errorText(error));
    }
  }, [applyRecord, refreshJobState]);

  useEffect(() => {
    void refreshList();
  // Initial native hydration only. Selection changes are loaded explicitly.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => meetingRecorder.subscribe((snapshot) => {
    setRecorder(snapshot);
    if (snapshot.meeting) {
      // Level and PCM progress belong to the recorder; browsing or editing
      // another meeting must retain its selection and unsaved draft.
      if (!selectedIdRef.current || selectedIdRef.current === snapshot.meeting.id) {
        selectedIdRef.current = snapshot.meeting.id;
        setSelectedId(snapshot.meeting.id);
        applyRecord(snapshot.meeting);
      }
      void refreshJobState(snapshot.meeting);
      setRecords((current) => {
        const without = current.filter((item) => item.id !== snapshot.meeting!.id);
        return [snapshot.meeting!, ...without];
      });
    }
  }), [applyRecord, refreshJobState]);

  useEffect(() => {
    const processing = selected && NATIVE_JOB_STATUSES.includes(selected.status);
    const selectedJobActive = selected ? activeJobs[selected.id] : false;
    if ((!processing || selectedJobActive === false) && !busy?.startsWith("process:")) return;
    const timer = window.setInterval(() => {
      if (!selectedId) return;
      void meetingGet(selectedId).then((record) => {
        if (selectedIdRef.current !== record.id) return;
        applyRecord(record);
        setRecords((current) => current.map((item) => item.id === record.id ? record : item));
        void refreshJobState(record);
      }).catch(() => {});
    }, 1_500);
    return () => window.clearInterval(timer);
  }, [activeJobs, applyRecord, busy, refreshJobState, selected, selectedId]);

  const chooseRecord = async (record: MeetingRecord) => {
    setChosenModelId(null);
    setShowCreate(false);
    setEditingMinutes(false);
    selectedIdRef.current = record.id;
    setSelectedId(record.id);
    applyRecord(record);
    void refreshJobState(record);
    setTab(record.minutes ? "minutes" : record.transcript.length > 0 ? "transcript" : "audio");
  };

  const requireProvider = (requireSpeech = true) => {
    if (selectedModel?.providerId && (!requireSpeech || capable)) {
      return { modelId: selectedModel.id, providerId: selectedModel.providerId };
    }
    onToast?.(selectedModel ? connection.error ?? "正在检测转写接口，请稍后重试" : "请先配置 MiniMax 模型");
    if (!selectedModel) onOpenModelSettings?.();
    return null;
  };

  const startRecording = async () => {
    const provider = requireProvider();
    if (!provider) return;
    setBusy("start");
    setCaptureError(null);
    try {
      const record = await meetingRecorder.start(title, provider.modelId, provider.providerId, recordingSource);
      await refreshList(record.id);
      setTab("audio");
    } catch (error) {
      await refreshList(null);
      setCaptureError(errorText(error));
      onToast?.(`开始录音失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const importAudio = async () => {
    const provider = requireProvider();
    if (!provider) return;
    const [source] = await filesystemPickFiles({
      title: "选择会议录音",
      extensions: ["wav", "aiff", "flac", "alac", "m4a", "mp3", "aac", "ogg"],
      multiple: false,
      maxFiles: 1,
    });
    if (!source) return;
    setBusy("import");
    try {
      const record = await meetingImportAudio(source, title, provider.modelId, provider.providerId);
      await refreshList(record.id);
      setTitle(defaultMeetingTitle());
      setTab("audio");
    } catch (error) {
      await refreshList(null);
      onToast?.(`导入录音失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const stopRecording = async () => {
    setBusy("stop");
    try {
      const record = await meetingRecorder.stop();
      await refreshList(record.id);
      setTitle(defaultMeetingTitle());
    } catch (error) {
      if (!meetingRecorder.current().meeting) await refreshList(null);
      onToast?.(`结束录音失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const togglePause = async () => {
    setBusy("pause");
    try {
      await meetingRecorder.setPaused(!recorder.paused);
    } catch (error) {
      onToast?.(`切换录音状态失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const processMeeting = async (record: MeetingRecord) => {
    const provider = requireProvider();
    if (!provider) return;
    setBusy(`process:${record.id}`);
    setActiveJobs((current) => ({ ...current, [record.id]: undefined }));
    setSelected({ ...record, status: "transcribing", error: undefined });
    setTab("transcript");
    try {
      const completed = await meetingProcess(record.id, provider);
      await refreshList(completed.id, record.id);
      if (selectedIdRef.current === record.id) setTab("minutes");
    } catch (error) {
      await refreshList(record.id, record.id);
      onToast?.(`处理失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const regenerate = async () => {
    if (!selected) return;
    if (hasTranscriptDraft) {
      onToast?.("请先保存转写校对，再生成会议纪要");
      return;
    }
    // Existing text only needs the chat connection; an unavailable ASR route
    // must not prevent users from recovering or regenerating their minutes.
    const provider = requireProvider(false);
    if (!provider) return;
    setBusy("regenerate");
    try {
      const record = await meetingRegenerateMinutes(selected.id, provider);
      await refreshList(record.id, record.id);
      if (selectedIdRef.current === record.id) setTab("minutes");
    } catch (error) {
      onToast?.(`重新生成失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const saveTranscript = async () => {
    if (!selected || transcriptConflict || busy || activeJobs[selected.id] !== false) return;
    const submitted = draftTranscript;
    setBusy("save-transcript");
    try {
      const record = await meetingUpdate(selected.id, { transcript: submitted,
        expectedContent: { transcript: selected.transcript, minutes: selected.minutes ?? null } });
      meetingDrafts.savedTranscript(record, submitted);
      await refreshList(record.id, record.id);
      onToast?.("校对已保存，请重新生成会议纪要");
    } catch (error) {
      await refreshList(selected.id, selected.id);
      onToast?.(`保存转写失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const changeTranscript = (update: (current: TranscriptSegment[]) => TranscriptSegment[]) => {
    if (!selected || transcriptConflict || busy || activeJobs[selected.id] !== false) return;
    const next = update(draftTranscript);
    meetingDrafts.editTranscript(selected, next);
    setDraftTranscript(next);
    setHasTranscriptDraft(meetingDrafts.hasTranscript(selected.id));
  };

  const saveMinutes = async () => {
    if (!selected || minutesConflict || hasTranscriptDraft || busy || activeJobs[selected.id] !== false) return;
    const submitted = draftMinutes;
    setBusy("save-minutes");
    try {
      const record = await meetingUpdate(selected.id, { minutes: submitted,
        expectedContent: { transcript: selected.transcript, minutes: selected.minutes ?? null } });
      meetingDrafts.savedMinutes(record, submitted);
      if (selectedIdRef.current === record.id) setEditingMinutes(false);
      await refreshList(record.id, record.id);
    } catch (error) {
      await refreshList(selected.id, selected.id);
      onToast?.(`保存纪要失败：${errorText(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const copyMinutes = async () => {
    const content = editingMinutes ? draftMinutes : selected?.minutes;
    if (!content) return;
    const copied = await copyShareText(content);
    onToast?.(copied ? "会议纪要已复制" : "复制失败，请使用导出保存会议纪要");
  };

  const openAudio = async (id: string) => {
    try { await meetingOpenAudio(id); }
    catch (error) { onToast?.(`打开录音失败：${errorText(error)}`); }
  };

  const doExport = async (kind: "audio" | "minutes" | "minutes_docx" | "minutes_pdf" | "transcript" | "srt") => {
    if (!selected) return;
    try {
      const path = await meetingExport(selected.id, kind);
      if (path) onToast?.(`已导出“${path.split(/[\\/]/).pop()}”`);
    } catch (error) {
      onToast?.(`导出失败：${errorText(error)}`);
    }
  };

  const removeMeeting = async () => {
    if (!selected || meetingRecorder.current().meeting?.id === selected.id && meetingRecorder.current().active) {
      onToast?.("请先结束正在进行的录音");
      return;
    }
    if (!window.confirm(`确定删除《${selected.title}》及其本地录音吗？文件会移入系统回收站。`)) return;
    try {
      await meetingDelete(selected.id);
      meetingDrafts.remove(selected.id);
      setSelectedId(null);
      setSelected(null);
      await refreshList(null);
    } catch (error) {
      onToast?.(`删除失败：${errorText(error)}`);
    }
  };

  const completedChunks = selected?.chunks.filter((chunk) => chunk.status === "completed").length ?? 0;
  const processingProgress = selected?.chunks.length
    ? Math.round(completedChunks / selected.chunks.length * 100)
    : 0;
  const recordingThis = recorder.active && recorder.meeting?.id === selected?.id;
  const selectedJobActive = selected ? activeJobs[selected.id] : false;
  const interruptedProcessing = Boolean(
    selected
    && ["transcribing", "summarizing"].includes(selected.status)
    && selectedJobActive === false,
  );
  const interruptedImport = Boolean(
    selected?.status === "importing" && selectedJobActive === false,
  );
  const speakerNames = useMemo(
    () => [...new Set(draftTranscript.map((segment) => segment.speaker))],
    [draftTranscript],
  );

  return (
    <div className="meeting-panel">
      <header className="meeting-panel__header">
        <div>
          <h1>录音转写</h1>
          <p>录音保存在本机；转写服务就绪后，可生成、校对和导出转写文本与会议纪要。</p>
        </div>
        <div className={`meeting-panel__provider${capable ? " is-ready" : ""}`}>
          <span>{capable ? "转写接口已就绪" : !selectedModel ? "需要配置 MiniMax 模型" : connection.state === "checking" ? "正在检测转写接口" : "转写接口不可用"}</span>
          {!selectedModel && <button type="button" onClick={onOpenModelSettings}>配置模型</button>}
          {selectedModel && connection.state === "unavailable" && <button type="button" onClick={connection.retry}>重新检测</button>}
        </div>
      </header>

      {selected && !recorder.active && (
        <div className="meeting-panel__create-compact">
          <span>正在查看「{selected.title}」</span>
          <button type="button" aria-expanded={showCreate} aria-controls={showCreate ? "meeting-create-form" : undefined} onClick={() => { setChosenModelId(null); setShowCreate((value) => !value); }}>
            {showCreate ? "收起新建区" : "新建录音或导入文件"}
          </button>
        </div>
      )}

      {(!selected || recorder.active || showCreate) && <section id="meeting-create-form" className="meeting-panel__create">
        <div className="meeting-source" aria-label="录音来源">
          <div className="meeting-source__heading"><strong>录制哪些声音</strong><span>选择这次会议需要保留的音频</span></div>
          <div className="meeting-source__options" role="group" aria-label="录音来源">
            {([
              { id: "microphone" as const, title: "麦克风", detail: "记录你说的话", Icon: Mic },
              { id: "system" as const, title: "系统声音", detail: "记录电脑播放的声音", Icon: MonitorPlay },
              { id: "both" as const, title: "两者都录", detail: "同时记录双方声音", Icon: Headphones },
            ]).map(({ id, title: label, detail, Icon }) => (
              <button key={id} className={`meeting-source__option${recordingSource === id ? " is-selected" : ""}`} type="button"
                aria-pressed={recordingSource === id} onClick={() => { setRecordingSource(id); setCaptureError(null); }}
                disabled={recorder.active || busy !== null || (id !== "microphone" && !captureSupport?.systemAudio)}>
                <span className="meeting-source__icon"><Icon size={19} strokeWidth={1.8} /></span>
                <span className="meeting-source__copy"><strong>{label}</strong><small>{detail}</small></span>
                <span className="meeting-source__check" aria-hidden="true" />
              </button>
            ))}
          </div>
          <p className="meeting-source__hint">
            {recordingSource === "both" ? "建议佩戴耳机，避免扬声器声音再次被麦克风收录。" : recordingSource === "system" ? "录制当前默认播放设备的声音；切换扬声器后请重新开始录音。" : "只录制麦克风，适合面对面会议或个人口述。"}
            {recordingSource !== "microphone" && captureSupport?.detail && ` ${captureSupport.detail}。`}
            {!captureSupport?.systemAudio && captureSupport?.detail && ` ${captureSupport.detail}。`}
          </p>
        </div>
        <label>
          <span>会议模型</span>
          <select aria-label="会议模型" value={selectedModel?.id ?? ""} onChange={(event) => setChosenModelId(event.target.value)} disabled={recorder.active || busy !== null}>
            {meetingModels.length === 0 && <option value="">请先配置 MiniMax 模型</option>}
            {meetingModels.map((model) => <option key={model.id} value={model.id}>{modelChoiceLabel(model, meetingModels)}</option>)}
          </select>
        </label>
        <label>
          <span>会议名称</span>
          <input value={title} maxLength={100} onChange={(event) => setTitle(event.target.value)} disabled={recorder.active} />
        </label>
        <button className="meeting-primary" type="button" onClick={() => void startRecording()} disabled={!capable || recorder.active || busy !== null}>
          {busy === "start" ? <Loader2 className="meeting-spin" size={17} /> : recordingSource === "microphone" ? <Mic size={17} /> : <MonitorPlay size={17} />}
          开始录音
        </button>
        <button className="meeting-secondary" type="button" onClick={() => void importAudio()} disabled={!capable || recorder.active || busy !== null}>
          {busy === "import" ? <Loader2 className="meeting-spin" size={17} /> : <Upload size={17} />}
          导入录音
        </button>
        {captureError && <small className="meeting-panel__connection-error" role="alert">录音未开始：{captureError}</small>}
        {connection.state === "unavailable" && <small className="meeting-panel__connection-error" role="alert">{connection.error}</small>}
        <small className={`meeting-panel__privacy${selectedModel?.insecureHttp ? " is-insecure" : ""}`}>
          录音默认仅保存在本机；开始前请确认已获得参会者同意。转写时向所选服务上传不超过 7 分钟的分片。
          {selectedModel?.insecureHttp && " 此连接使用 HTTP，录音和 API Key 将通过明文网络传输。"}
        </small>
      </section>}

      {recorder.active && recorder.meeting && (
        <section className="meeting-recorder" aria-live="polite">
          <div className="meeting-recorder__pulse" style={{ transform: `scale(${0.8 + recorder.level * 0.5})` }}>{recorder.source === "microphone" ? <Mic size={20} /> : <MonitorPlay size={20} />}</div>
          <div className="meeting-recorder__main">
            <strong>{recorder.pendingSave ? "录音已停止，等待保存" : recorder.error ? recorder.source === "microphone" ? "录音已暂停，请处理保存问题" : "录音已中断，请结束并保存" : recorder.paused ? "录音已暂停" : "正在录音并自动保存"}</strong>
            <span>{recorder.meeting.title} · {recorder.source ? SOURCE_LABELS[recorder.source] : "录音"} · {formatMeetingDuration(recorder.meeting.durationSeconds)}</span>
            {recorder.error && <em>{recorder.error}</em>}
          </div>
          <button className="meeting-secondary" type="button" onClick={() => void togglePause()} disabled={busy !== null || recorder.pendingSave || (Boolean(recorder.error) && recorder.source !== "microphone")}>
            {recorder.paused ? <Play size={16} /> : <Pause size={16} />}{recorder.paused ? recorder.error && !recorder.pendingSave ? "重试并继续" : "继续" : "暂停"}
          </button>
          <button className="meeting-danger" type="button" onClick={() => void stopRecording()} disabled={busy !== null}>
            {busy === "stop" ? <Loader2 className="meeting-spin" size={16} /> : <CircleStop size={16} />}{recorder.pendingSave ? "重试保存" : "结束并保存"}
          </button>
        </section>
      )}

      <div className="meeting-panel__body">
        <aside className="meeting-list">
          <div className="meeting-list__heading"><span>最近会议</span><button type="button" aria-label="刷新会议列表" onClick={() => void refreshList()}><RefreshCw size={14} /></button></div>
          {loadError && <div className="meeting-empty is-error">{loadError}</div>}
          {!loadError && records.length === 0 && <div className="meeting-empty"><FileAudio size={30} /><span>还没有会议记录</span></div>}
          {records.map((record) => (
            <button key={record.id} type="button" className={`meeting-list__item${selectedId === record.id ? " is-active" : ""}`} onClick={() => void chooseRecord(record)}>
              <span className="meeting-list__title">{record.title}</span>
              <span className="meeting-list__meta"><Clock3 size={12} />{formatMeetingDuration(record.durationSeconds)} · {STATUS_LABELS[record.status] ?? record.status}{record.captureSource && record.captureSource !== "imported" ? ` · ${SOURCE_SHORT_LABELS[record.captureSource]}` : ""}</span>
              {record.status === "recording" && <i />}
            </button>
          ))}
        </aside>

        <main className="meeting-detail">
          {meetingDrafts.persistenceError() && <div className="meeting-error" role="alert">
            {meetingDrafts.persistenceError()}
            <button type="button" onClick={() => {
              try { meetingDrafts.ensureDurable();  }
              catch (error) { onToast?.(String(error).replace(/^Error:\s*/, "")); }
              if (selected) applyRecord(selected);
              setDraftRevision(value => value + 1);
            }}>重试保存草稿</button>
            {meetingDrafts.recoveryBytes() !== null && <details>
              <summary>恢复原始草稿数据</summary>
              <p>可复制原始数据自行恢复；重新开始前会保留本地恢复副本。</p>
              <textarea readOnly aria-label="原始会议草稿数据" value={meetingDrafts.recoveryBytes() ?? ""} />
              <button type="button" onClick={() => {
                if (!window.confirm("已确认保留原始数据后，重新开始会议草稿？本地将保留恢复副本。")) return;
                try { meetingDrafts.resetUnreadable(); if (selected) applyRecord(selected); setDraftRevision(value => value + 1); }
                catch (error) { onToast?.(`无法重新开始：${String(error)}`); }
              }}>保留恢复副本并重新开始</button>
            </details>}
          </div>}
          {!selected && <div className="meeting-detail__empty"><Mic size={42} /><h2>记录每一次重要讨论</h2><p>开始录音或导入已有音频。两小时以上会议会在本地自动分段处理。</p></div>}
          {selected && (
            <>
              <header className="meeting-detail__header">
                <div>
                  <h2>{selected.title}</h2>
                  <p>{new Date(selected.createdAt).toLocaleString("zh-CN")} · {formatMeetingDuration(selected.durationSeconds)} · {STATUS_LABELS[selected.status] ?? selected.status}{selected.captureSource && selected.captureSource !== "imported" ? ` · ${SOURCE_LABELS[selected.captureSource]}` : ""}</p>
                </div>
                <div className="meeting-detail__actions">
                  <button type="button" onClick={() => void openAudio(selected.id)}><Play size={14} />系统播放器</button>
                  <button type="button" onClick={() => void doExport("audio")}><Download size={14} />下载录音</button>
                  <button className="is-danger" type="button" onClick={() => void removeMeeting()} disabled={NATIVE_JOB_STATUSES.includes(selected.status) && selectedJobActive !== false} aria-label="删除会议记录"><Trash2 size={14} /></button>
                </div>
              </header>

              {!recordingThis && <div className="meeting-callout meeting-processing-model">
                <label>
                  <span>本次处理模型</span>
                  <select aria-label="本次处理模型" value={selectedModel?.id ?? ""} onChange={(event) => setChosenModelId(event.target.value)} disabled={busy !== null || (NATIVE_JOB_STATUSES.includes(selected.status) && selectedJobActive !== false)}>
                    {meetingModels.length === 0 && <option value="">请先配置 MiniMax 模型</option>}
                    {meetingModels.map((model) => <option key={model.id} value={model.id}>{modelChoiceLabel(model, meetingModels)}</option>)}
                  </select>
                </label>
                <small className={`meeting-panel__privacy${selectedModel?.insecureHttp ? " is-insecure" : ""}`}>
                  {selected.modelId && !meetingModels.some((model) => model.id === selected.modelId && model.providerId === selected.providerId) && "原会议连接已不可用，请确认本次处理模型。"}
                  {selectedModel ? ` 转写及纪要将使用「${modelChoiceLabel(selectedModel, meetingModels)}」，开始处理后保存此选择。` : "请配置模型后重试，已保存的录音和转写仍可导出。"}
                  {selectedModel?.insecureHttp && " 此连接使用 HTTP，录音和 API Key 将通过明文网络传输。"}
                  {connection.state === "unavailable" && ` ${connection.error}${selected.transcript.length ? "；已有转写仍可用于生成纪要。" : ""}`}
                </small>
              </div>}

              {(["recording", "paused", "recorded", "partial", "failed"].includes(selected.status) || interruptedProcessing
                || selectedJobActive === false && selected.chunks.some((chunk) => chunk.status !== "completed")) && !recordingThis && selected.recordedSamples > 0 && (
                <div className="meeting-callout">
                  <div><Sparkles size={20} /><span><strong>{interruptedProcessing ? "上次处理已中断" : "录音已安全保存"}</strong><small>{interruptedProcessing ? "已完成的分片仍保存在本机，可以从断点继续。" : "将按 7 分钟切片转写，完成后自动生成会议纪要。"}</small></span></div>
                  <button className="meeting-primary" type="button" onClick={() => void processMeeting(selected)} disabled={busy !== null}>{interruptedProcessing ? "继续处理" : "开始转写并生成纪要"}</button>
                </div>
              )}

              {NATIVE_JOB_STATUSES.includes(selected.status) && selectedJobActive !== false && (
                <div className="meeting-progress">
                  <div><Loader2 className="meeting-spin" size={18} /><span>{selected.status === "importing" ? "正在导入并转换录音…" : selected.status === "summarizing" ? "正在生成会议纪要…" : `正在转写录音… ${processingProgress}%`}</span></div>
                  <div className="meeting-progress__bar"><i style={{ width: `${selected.status === "summarizing" ? 95 : processingProgress}%` }} /></div>
                  <small>可以离开此页面，录音和已完成的转写不会丢失。</small>
                </div>
              )}

              {interruptedImport && <div className="meeting-error">录音导入被意外中断，请删除这条未完成记录后重新导入。</div>}

              {selected.error && <div className="meeting-error">{selected.error}</div>}

              {transcriptConflict && <div className="meeting-draft-notice" role="alert">
                <p>最新转写与原校对草稿有冲突。已保留原草稿，确认前不会覆盖已保存内容。
                  {!transcriptConflict.canKeep && " 部分段落已重新划分，请先复制原草稿，再选择使用最新转写并重新校对。"}</p>
                <div>
                  <button type="button" disabled={busy !== null} onClick={() => void copyShareText(transcriptConflict.original)
                    .then((copied) => onToast?.(copied ? "原校对草稿已复制" : "复制失败，请重试"))}>复制原校对草稿</button>
                  {transcriptConflict.canKeep && <button type="button" disabled={busy !== null || selectedJobActive !== false} onClick={() => {
                    meetingDrafts.acceptTranscript(selected);
                    applyRecord(selected);
                    setTab("transcript");
                  }}>保留我的校对并合并新段落</button>}
                  <button type="button" disabled={busy !== null || selectedJobActive !== false} onClick={() => {
                    meetingDrafts.discardTranscript(selected.id);
                    applyRecord(selected);
                    setTab("transcript");
                  }}>舍弃校对，使用最新转写</button>
                </div>
              </div>}
              {minutesConflict && <div className="meeting-draft-notice" role="alert">
                <p>纪要或转写已更新，原纪要草稿已保留。当前预览为最新已保存内容，请核对后选择。</p>
                <div>
                  <button type="button" disabled={busy !== null} onClick={() => void copyShareText(minutesConflict.original)
                    .then((copied) => onToast?.(copied ? "原纪要草稿已复制" : "复制失败，请重试"))}>复制原纪要草稿</button>
                  <button type="button" disabled={busy !== null || selectedJobActive !== false || hasTranscriptDraft} onClick={() => {
                    meetingDrafts.acceptMinutes(selected);
                    applyRecord(selected);
                    setEditingMinutes(true);
                    setTab("minutes");
                  }}>保留原草稿并编辑</button>
                  <button type="button" disabled={busy !== null || selectedJobActive !== false} onClick={() => {
                    meetingDrafts.discardMinutes(selected.id);
                    applyRecord(selected);
                  }}>舍弃草稿，使用最新纪要</button>
                </div>
              </div>}

              <nav className="meeting-tabs" aria-label="会议内容">
                <button type="button" className={tab === "minutes" ? "is-active" : ""} onClick={() => setTab("minutes")}>会议纪要</button>
                <button type="button" className={tab === "transcript" ? "is-active" : ""} onClick={() => setTab("transcript")}>完整转写 <span>{selected.transcript.length || ""}</span></button>
                <button type="button" className={tab === "audio" ? "is-active" : ""} onClick={() => setTab("audio")}>录音</button>
              </nav>

              <div className="meeting-tab-content">
                {tab === "minutes" && (
                  selected.minutes || editingMinutes ? (
                    <div className="meeting-minutes">
                      <div className="meeting-content-toolbar">
                        <button type="button" onClick={() => void copyMinutes()}><Copy size={14} />复制</button>
                        <button type="button" disabled={hasMinutesDraft || hasTranscriptDraft || busy !== null} onClick={() => void doExport("minutes_docx")}><Download size={14} />导出 Word</button>
                        <button type="button" disabled={hasMinutesDraft || hasTranscriptDraft || busy !== null} onClick={() => void doExport("minutes_pdf")}><Download size={14} />导出 PDF</button>
                        <button type="button" disabled={hasMinutesDraft || hasTranscriptDraft || busy !== null} onClick={() => void doExport("minutes")}><Download size={14} />导出 Markdown</button>
                        <button type="button" onClick={() => {
                          setDraftMinutes(meetingDrafts.minutes(selected).value);
                          setEditingMinutes(true);
                        }} disabled={busy !== null || selectedJobActive !== false || !!minutesConflict || hasTranscriptDraft}>编辑纪要</button>
                        <button type="button" onClick={() => void regenerate()} disabled={busy !== null || editingMinutes || hasTranscriptDraft}><RefreshCw size={14} />重新生成</button>
                      </div>
                      {hasTranscriptDraft && <p className="meeting-draft-hint" role="status">转写校对尚未保存；请先保存并重新生成纪要，再导出最新内容。</p>}
                      {hasMinutesDraft && !minutesConflict && <p className="meeting-draft-hint" role="status">{meetingDrafts.persistenceError() ? "纪要草稿尚未保存在此设备。" : "纪要草稿已保存在此设备，重启后可继续。"}请保存后再导出；{editingMinutes ? "正在编辑草稿。" : "预览显示已保存内容。"}</p>}
                      {editingMinutes ? (
                        <div className="meeting-minutes__editor">
                          <textarea aria-label="编辑会议纪要" rows={18} value={draftMinutes} disabled={busy !== null || selectedJobActive !== false || hasTranscriptDraft} onChange={(event) => {
                            setDraftMinutes(event.target.value);
                            meetingDrafts.editMinutes(selected, event.target.value);
                            setHasMinutesDraft(meetingDrafts.hasMinutes(selected.id));
                          }} />
                          <button className="meeting-primary" type="button" onClick={() => void saveMinutes()} disabled={busy !== null || selectedJobActive !== false || !!minutesConflict || hasTranscriptDraft || !draftMinutes.trim()}>保存纪要</button>
                          <button type="button" onClick={() => setEditingMinutes(false)} disabled={busy !== null}>返回预览（保留草稿）</button>
                        </div>
                      ) : <Markdown complete markdownTheme="loose">{selected.minutes ?? ""}</Markdown>}
                    </div>
                  ) : (
                    <div className="meeting-tab-empty"><Sparkles size={34} /><h3>尚未生成会议纪要</h3><p>{hasTranscriptDraft ? "请先在完整转写中保存校对，再生成会议纪要。" : "完成录音转写后，会自动整理结论、待办、风险和未决问题。"}</p>{selected.transcript.length > 0 && <button className="meeting-primary" type="button" disabled={busy !== null || hasTranscriptDraft} onClick={() => void regenerate()}>生成会议纪要</button>}</div>
                  )
                )}

                {tab === "transcript" && (
                  draftTranscript.length > 0 ? (
                    <div className="meeting-transcript">
                      <div className="meeting-content-toolbar">
                        <span>{speakerNames.length} 位说话人 · 可直接校对</span>
                        <button type="button" disabled={hasTranscriptDraft || busy !== null} onClick={() => void doExport("transcript")}><Download size={14} />TXT</button>
                        <button type="button" disabled={hasTranscriptDraft || busy !== null} onClick={() => void doExport("srt")}><Download size={14} />SRT</button>
                        <button className="meeting-primary" type="button" onClick={() => void saveTranscript()} disabled={busy !== null || !!transcriptConflict || selectedJobActive !== false}>保存校对</button>
                      </div>
                      {hasTranscriptDraft && !transcriptConflict && <p className="meeting-draft-hint" role="status">{meetingDrafts.persistenceError() ? "校对草稿尚未保存在此设备。" : "校对草稿已保存在此设备，重启后可继续。"}保存后即可导出最新转写。</p>}
                      <div className="meeting-transcript__rows">
                        {draftTranscript.map((segment, index) => (
                          <div className="meeting-transcript__row" key={`${segment.id}-${index}`}>
                            <button type="button" className="meeting-transcript__time" title="在系统播放器中打开录音" onClick={() => void openAudio(selected.id)}>{formatMeetingDuration(segment.start)}</button>
                            <input aria-label={`第 ${index + 1} 段说话人`} disabled={busy !== null || !!transcriptConflict || selectedJobActive !== false} value={segment.speaker} onChange={(event) => {
                              const previous = segment.speaker;
                              const next = event.target.value;
                              changeTranscript((current) => current.map((item) => item.speaker === previous ? { ...item, speaker: next } : item));
                            }} />
                            <textarea aria-label={`第 ${index + 1} 段转写`} disabled={busy !== null || !!transcriptConflict || selectedJobActive !== false} value={segment.text} rows={Math.max(1, Math.ceil(segment.text.length / 60))} onChange={(event) => changeTranscript((current) => current.map((item, itemIndex) => itemIndex === index ? { ...item, text: event.target.value } : item))} />
                          </div>
                        ))}
                      </div>
                    </div>
                  ) : (
                    <div className="meeting-tab-empty"><FileText size={34} /><h3>尚未生成转写</h3><p>录音保存在本地，开始转写后才会上传分片到所选服务。</p></div>
                  )
                )}

                {tab === "audio" && (
                  <div className="meeting-audio">
                    <div className="meeting-audio__icon"><FileAudio size={36} /></div>
                    <div><h3>完整录音已保存在本地</h3><p>{selected.originalFileName ? `原始文件：${selected.originalFileName} · ` : ""}标准化副本：16 kHz 单声道 WAV · {formatMeetingDuration(selected.durationSeconds)}</p></div>
                    <button className="meeting-secondary" type="button" onClick={() => void openAudio(selected.id)}><Play size={15} />打开播放</button>
                    <button className="meeting-primary" type="button" onClick={() => void doExport("audio")}><Download size={15} />下载录音</button>
                  </div>
                )}
              </div>
            </>
          )}
        </main>
      </div>
    </div>
  );
}
