import { isRemovedModelId, isRemovedModelProvider } from "@/lib/retired-models";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import {
  CircleStop,
  Copy,
  Download,
  FileText,
  Headphones,
  Loader2,
  Mic,
  MonitorPlay,
  Pause,
  Play,
  Sparkles,
  Trash2,
  PanelLeft,
  Plus,
  MoreHorizontal,
  Search,
  Pencil,
  ArrowDown,
} from "lucide-react";
import { copyShareText } from "@/lib/share";
import { meetingDrafts, type MeetingDraftConflict } from "@/lib/meeting-drafts";
import { meetingTranscriptionIssue } from '@/lib/meeting-transcription-error';
import {
  formatMeetingDuration,
  meetingCaptureSupport,
  meetingDelete,
  isMiniMaxMeetingModel,
  meetingExport,
  meetingGet,
  meetingJobActive,
  meetingList,
  meetingProcess,
  meetingRecorder,
  meetingRegenerateMinutes,
  meetingUpdate,
  meetingRestoreMinutes,
  type MeetingRecord,
  type RecordingSource,
  type CaptureSupport,
  type TranscriptSegment,
} from "@/lib/meeting-minutes";
import { PageToolbar } from "./PageToolbar";
import { MeetingAudioPlayer, type MeetingSeek, type MeetingPlayerHandle } from "./MeetingAudioPlayer";
import { MeetingTranscriptPassage } from "./MeetingTranscriptPassage";
import type { ModelOption } from "./ModelSelector";
import { Markdown } from "./markdown/Markdown";

interface MeetingMinutesPanelProps {
  modelId?: string;
  models: ModelOption[];
  onToast?: (message: string) => void;
  onOpenModelSettings?: () => void;
}

type DetailTab = "minutes" | "transcript";

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
  const tabPrefix = useId().replace(/:/g, "");
  const [selected, setSelected] = useState<MeetingRecord | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const meetingModels = models.filter((model) => model.providerId
    && !isRemovedModelProvider({ id: model.providerId ?? "", source: model.source })
    && !isRemovedModelId(model.id)
    && isMiniMaxMeetingModel(model));
  const selectedModel = (!showCreate ? meetingModels.find((model) => model.id === selected?.modelId && model.providerId === selected.providerId) : undefined)
    ?? meetingModels.find((model) => model.id === modelId)
    ?? meetingModels[0];
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
  const [connectionError,setConnectionError]=useState('');
  const [, setDraftRevision] = useState(0);
  const [draftTranscript, setDraftTranscript] = useState<TranscriptSegment[]>([]);
  const [recorder, setRecorder] = useState(meetingRecorder.current());
  const [activeJobs, setActiveJobs] = useState<Record<string, boolean | undefined>>({});
  const [recordingSource, setRecordingSource] = useState<RecordingSource>(() => { try { const stored = localStorage.getItem("echo.meeting.recording-source"); if (stored === "system" || stored === "both") return stored; } catch { /* use microphone */ } return "microphone"; });
  const [captureSupport, setCaptureSupport] = useState<CaptureSupport | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(true);
  const [listQuery, setListQuery] = useState("");
  const [query, setQuery] = useState("");
  const [editingTranscript, setEditingTranscript] = useState(false);
  const [seekRequest, setSeekRequest] = useState<MeetingSeek>();
  const playerControl = useRef<MeetingPlayerHandle | null>(null);
  const seekSequence = useRef(0);
  const [playPosition, setPlayPosition] = useState(-1);
  const [follow, setFollow] = useState(true);
  const [menu, setMenu] = useState<"export" | "more" | null>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const workbenchRef = useRef<HTMLDivElement>(null);
  const rowRefs = useRef(new Map<number, HTMLDivElement>());
  const tabTouched = useRef(false);

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
    void meetingCaptureSupport().then(support => { setCaptureSupport(support); if (!support.systemAudio) setRecordingSource("microphone"); }).catch(() => { setCaptureSupport({ systemAudio: false, detail: "无法检测系统声音采集能力" }); setRecordingSource("microphone"); });
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
        if (!selectedIdRef.current) setTab(record.minutes ? "minutes" : "transcript");
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
        if (!snapshot.active && snapshot.meeting.minutes && !tabTouched.current) setTab("minutes");
      }
      void refreshJobState(snapshot.meeting);
      setRecords((current) => {
        const without = current.filter((item) => item.id !== snapshot.meeting!.id);
        return [snapshot.meeting!, ...without];
      });
    }
  }), [applyRecord, refreshJobState]);

  useEffect(() => {
    const processing = selected && (NATIVE_JOB_STATUSES.includes(selected.status) || recorder.processingIds?.includes(selected.id));
    const selectedJobActive = selected ? activeJobs[selected.id] : false;
    if ((!processing || selectedJobActive === false && !recorder.processingIds?.includes(selected?.id ?? "")) && !busy?.startsWith("process:")) return;
    const timer = window.setInterval(() => {
      if (!selectedId) return;
      void meetingGet(selectedId).then((record) => {
        if (selectedIdRef.current !== record.id) return;
        applyRecord(record);
        if (record.minutes && !tabTouched.current && !editingMinutes && !editingTranscript) setTab("minutes");
        setRecords((current) => current.map((item) => item.id === record.id ? record : item));
        void refreshJobState(record);
      }).catch(() => {});
    }, 1_500);
    return () => window.clearInterval(timer);
  }, [activeJobs[selectedId ?? ""], applyRecord, busy, refreshJobState, selected?.id, selected?.status, selectedId, recorder.processingIds, editingMinutes, editingTranscript]);

  const chooseRecord = async (record: MeetingRecord) => {
    setShowCreate(false);
    setEditingMinutes(false);
    setEditingTranscript(false); setMenu(null); setQuery(""); setPlayPosition(-1); setSeekRequest(undefined); tabTouched.current = false;
    selectedIdRef.current = record.id;
    setSelectedId(record.id);
    applyRecord(record);
    void refreshJobState(record);
    setTab(record.minutes ? "minutes" : "transcript");
  };

  const requireProvider = () => {
    if (selectedModel?.providerId) {
      return { modelId: selectedModel.id, providerId: selectedModel.providerId };
    }
    setConnectionError('尚未配置可用的录音转写连接。请在模型设置中配置支持转写的 MiniMax 连接，再重试；本机录音和原文会保留。');
    onToast?.('请先配置可用的录音转写连接，本机录音已保留');
    return null;
  };

  useEffect(()=>{if(selectedModel?.providerId)setConnectionError('');},[selectedModel?.id,selectedModel?.providerId]);
  const retryTranscription=async()=>{
    const binding=requireProvider();if(!binding)return;
    setBusy('retry-transcription');
    try{await meetingRecorder.retryTranscription(binding);setConnectionError('');}
    catch(error){setConnectionError(errorText(error));onToast?.(`重试转写失败：${errorText(error)}`);}
    finally{setBusy(null);}
  };

  const startRecording = async () => {
    setBusy("start");
    setCaptureError(null);
    try {
      const record = await meetingRecorder.start(title, selectedModel?.id ?? "", selectedModel?.providerId ?? "", recordingSource);
      await refreshList(record.id);
      setTab("transcript"); setShowCreate(false); setLibraryOpen(false); setFollow(true); tabTouched.current = false;
      try { localStorage.setItem("echo.meeting.recording-source", recordingSource); } catch { /* recording remains usable in private storage */ }
    } catch (error) {
      await refreshList(null);
      setCaptureError(errorText(error));
      onToast?.(`开始录音失败：${errorText(error)}`);
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
    const provider = requireProvider();
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
      setEditingTranscript(false);
      return record;
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
      return record;
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

  const doExport = async (kind: "audio" | "minutes" | "minutes_docx" | "minutes_pdf" | "transcript" | "srt") => {
    if (!selected) return;
    try {
      if (kind.startsWith("minutes") && hasMinutesDraft && !await saveMinutes()) return;
      if ((kind === "transcript" || kind === "srt") && hasTranscriptDraft && !await saveTranscript()) return;
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

  const recordingThis = recorder.active && recorder.meeting?.id === selected?.id;
  const processingThis = Boolean(selected && (recorder.processingIds?.includes(selected.id) || NATIVE_JOB_STATUSES.includes(selected.status) && activeJobs[selected.id] !== false));
  const selectedJobActive = selected ? activeJobs[selected.id] : false;
  const readOnly = recordingThis || processingThis || busy !== null || selectedJobActive === undefined;
  const needsTranscription = Boolean(selected && (selected.recordedSamples > 0 && !selected.transcript.length || ["recorded", "partial", "failed", "interrupted", "importing", "transcribing"].includes(selected.status) || selected.chunks.some(chunk => chunk.status !== "completed")));
  const seekTo = useCallback((time: number, autoplay: boolean) => {
    setTab("transcript"); setQuery(""); setPlayPosition(time); setFollow(true);
    const request = { time, autoplay, sequence: ++seekSequence.current };
    playerControl.current?.seek(request);
    setSeekRequest(request);
    tabTouched.current = true;
  }, []);
  const activeSegment = selected?.transcript.find(segment => playPosition >= segment.start && playPosition < segment.end)?.id;
  useEffect(() => {
    if (!follow || editingTranscript || tab !== "transcript") return;
    const target = recordingThis ? selected?.transcript[selected.transcript.length - 1]?.id : activeSegment;
    if (target !== undefined) rowRefs.current.get(target)?.scrollIntoView?.({ block: "nearest" });
  }, [follow, editingTranscript, tab, recordingThis, selected?.transcript, activeSegment]);
  useEffect(() => {
    if (recorder.active) setLibraryOpen(false);
  }, [recorder.active]);
  useEffect(() => {
    if (!workbenchRef.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(entries => { if (entries[0]?.contentRect.width < 780) setLibraryOpen(false); });
    observer.observe(workbenchRef.current);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (recorder.processingIds) void refreshList();
  }, [recorder.processingIds, refreshList]);

  const minutesMarkup = useMemo(() => (selected?.minutes ?? "").replace(/\[(\d{2}:\d{2}:\d{2})\](?!\()/g, (match, stamp: string) => {
    const parts = stamp.split(":").map(Number);
    const time = parts[0] * 3600 + parts[1] * 60 + parts[2];
    if (!selected?.transcript.some(segment => time >= Math.floor(segment.start) && time <= Math.ceil(segment.end))) return match;
    return `[${stamp}](#meeting-time-${time})`;
  }), [selected?.minutes, selected?.transcript]);
  const transcriptRows = useMemo(() => (editingTranscript ? draftTranscript : selected?.transcript ?? []).filter(segment => !query || `${segment.speaker} ${segment.text}`.toLocaleLowerCase().includes(query.toLocaleLowerCase())), [editingTranscript, draftTranscript, selected?.transcript, query]);
  const changeRef = useRef(changeTranscript); changeRef.current = changeTranscript;
  const changeText = useCallback((id: number, text: string) => changeRef.current(current => current.map(item => item.id === id ? { ...item, text } : item)), []);
  const changeSpeaker = useCallback((speaker: string, name: string) => changeRef.current(current => current.map(item => item.speaker === speaker ? { ...item, speaker: name } : item)), []);
  const registerRow = useCallback((id: number, node: HTMLDivElement | null) => { if (node) rowRefs.current.set(id, node); else rowRefs.current.delete(id); }, []);
  const beginCreate = () => { setShowCreate(true); setLibraryOpen(false); setMenu(null); setCaptureError(null); };
  const switchTab = (next: DetailTab) => { setTab(next); setMenu(null); tabTouched.current = true; };
  const exportedDraft = tab === "minutes" ? hasMinutesDraft : hasTranscriptDraft;
  const transcriptionIssue=meetingTranscriptionIssue(recorder.transcription?.error);

  return <div ref={workbenchRef} className={`meeting-workbench${libraryOpen ? " meeting-workbench--library" : ""}`}>
    <PageToolbar className="meeting-workbench__toolbar">
      <button type="button" className="meeting-icon-button" aria-label={libraryOpen ? "收起录音列表" : "展开录音列表"} aria-expanded={libraryOpen} aria-controls="meeting-library" onClick={() => setLibraryOpen(value => !value)}><PanelLeft size={18} /></button>
      <h1>录音转写</h1>
      {recorder.active && !recordingThis && <button type="button" onClick={() => recorder.meeting && void chooseRecord(recorder.meeting)}>返回正在录音</button>}
      <button type="button" className="meeting-primary meeting-workbench__new" onClick={beginCreate} disabled={recorder.active || busy === "start"}><Plus size={16} />新建录音</button>
    </PageToolbar>
    {loadError && <div className="meeting-workbench__notice" role="alert">录音列表读取失败：{loadError}<button type="button" onClick={() => void refreshList()}>重试</button></div>}
    {connectionError && <div className="meeting-workbench__notice" role="alert"><span>{connectionError}</span>{onOpenModelSettings && <button type="button" onClick={onOpenModelSettings}>检查模型连接</button>}</div>}
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

    <div className="meeting-workbench__body">
      {libraryOpen && <aside className="meeting-library" id="meeting-library" aria-label="录音列表">
        <div className="meeting-library__heading"><strong>录音列表</strong><span>{records.length}</span></div>
        <label className="meeting-search"><Search size={15} /><input aria-label="搜索录音" placeholder="搜索录音名称" value={listQuery} onChange={event => setListQuery(event.target.value)} /></label>
        <div className="meeting-library__items">
          {records.filter(record => record.title.toLocaleLowerCase().includes(listQuery.toLocaleLowerCase())).map(record => <button type="button" key={record.id} className={`meeting-library__item${selected?.id === record.id && !showCreate ? " is-selected" : ""}`} aria-current={selected?.id === record.id && !showCreate ? "true" : undefined} onClick={() => void chooseRecord(record)}>
            <strong>{record.title}</strong><span>{formatMeetingDuration(record.durationSeconds)} · {recorder.processingIds?.includes(record.id) ? "正在整理" : STATUS_LABELS[record.status] ?? record.status}</span><small>{new Date(record.createdAt).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })}</small>
          </button>)}
          {!records.length && <p className="meeting-muted">录音会自动保存在这里</p>}
          {!!records.length && !records.some(record => record.title.toLocaleLowerCase().includes(listQuery.toLocaleLowerCase())) && <p className="meeting-muted">没有找到匹配的录音</p>}
        </div>
      </aside>}
      <section className="meeting-workbench__detail" aria-label="当前录音">
        {(showCreate || !selected) && !recorder.active ? <div className="meeting-start">
          <h2>{records.length ? "新建录音" : "记录讨论，专注交流"}</h2><p className="meeting-muted">录制时按短句显示文字，结束后自动整理纪要。</p>
          <fieldset className="meeting-start__sources"><legend>录制哪些声音</legend>
            {([
              { id: "microphone" as const, label: "麦克风", detail: "面对面会议、个人口述", Icon: Mic },
              { id: "system" as const, label: "电脑声音", detail: "电脑播放的声音", Icon: MonitorPlay },
              { id: "both" as const, label: "两者都录", detail: "线上会议，记录双方", Icon: Headphones },
            ]).map(({ id, label, detail, Icon }) => <button type="button" key={id} className={`meeting-start__source${recordingSource === id ? " is-selected" : ""}`} aria-pressed={recordingSource === id} disabled={busy === "start" || id !== "microphone" && !captureSupport?.systemAudio} onClick={() => { setRecordingSource(id); setCaptureError(null); }}><Icon size={19} /><strong>{label}</strong><small>{detail}</small></button>)}
          </fieldset>
          <p className="meeting-muted">{recordingSource === "both" ? "建议佩戴耳机，避免声音重复收录。" : recordingSource === "system" ? "录制当前默认播放设备的声音。" : "使用当前默认麦克风。"}{captureSupport?.detail && ` ${captureSupport.detail}。`}</p>
          <p className="meeting-muted">音频保存在本机，转写需要联网并向现有服务发送录音片段。</p>
          {!selectedModel && <p className="meeting-muted">尚未配置转写连接，可以先录音，配置后再补转。{onOpenModelSettings && <button type="button" onClick={onOpenModelSettings}>配置转写连接</button>}</p>}
          {captureError && <p className="meeting-error-text" role="alert">录音未开始：{captureError}</p>}
          <div className="meeting-start__actions"><button type="button" className="meeting-primary" disabled={busy === "start" || recordingSource !== "microphone" && !captureSupport?.systemAudio} onClick={() => void startRecording()}>{busy === "start" ? <Loader2 size={16} className="meeting-spin" /> : <Mic size={16} />}开始录音</button>{selected && <button type="button" disabled={busy === "start"} onClick={() => setShowCreate(false)}>返回录音</button>}</div>
        </div> : selected && <>
          <header className="meeting-workbench__record-header">
            <div><h2>{selected.title}</h2><p className="meeting-muted">{new Date(selected.createdAt).toLocaleString("zh-CN")} · {recordingThis ? SOURCE_LABELS[recorder.source ?? "microphone"] : `${formatMeetingDuration(selected.durationSeconds)} · ${STATUS_LABELS[selected.status] ?? selected.status}`}</p></div>
            {recordingThis ? <span className="meeting-live-status" role="status">{recorder.pendingSave ? "等待保存" : recorder.paused ? "已暂停" : "录音中"} · {recorder.transcription?.state === "unavailable" ? "转写等待恢复" : recorder.transcription?.state === "recognizing" ? "正在识别短句" : "实时转写"}</span> : <div className="meeting-workbench__record-actions">
              <button type="button" onClick={() => void (tab === "minutes" ? copyMinutes() : copyShareText((editingTranscript ? draftTranscript : selected.transcript).map(segment => `${segment.speaker}: ${segment.text}`).join("\n")).then(ok => onToast?.(ok ? "原文已复制" : "复制失败，请重试")))} disabled={tab === "minutes" && !selected.minutes}><Copy size={14} />复制</button>
              <button type="button" aria-expanded={menu === "export"} onClick={() => setMenu(value => value === "export" ? null : "export")}><Download size={14} />{exportedDraft ? "保存并导出" : "导出"}</button>
              <button type="button" className="meeting-icon-button" aria-label="更多录音操作" aria-expanded={menu === "more"} onClick={() => setMenu(value => value === "more" ? null : "more")}><MoreHorizontal size={18} /></button>
            </div>}
          </header>
          {menu === "export" && <div className="meeting-workbench__menu" aria-label="导出选项">
            {selected.minutes && <>{(["minutes_docx", "minutes_pdf", "minutes"] as const).map((kind, index) => <button key={kind} type="button" disabled={readOnly || hasTranscriptDraft || !!minutesConflict} onClick={() => void doExport(kind)}>{["Word", "PDF", "Markdown"][index]}</button>)}</>}
            <button type="button" disabled={readOnly || !selected.transcript.length || !!transcriptConflict} onClick={() => void doExport("transcript")}>原文 TXT</button><button type="button" disabled={readOnly || !selected.transcript.length || !!transcriptConflict} onClick={() => void doExport("srt")}>字幕 SRT</button><button type="button" disabled={!selected.recordedSamples} onClick={() => void doExport("audio")}>音频</button>
            {hasTranscriptDraft && <span className="meeting-muted">原文尚有校对草稿，请保存原文并更新纪要后再导出纪要。</span>}
          </div>}
          {menu === "more" && <div className="meeting-workbench__menu" aria-label="更多录音操作">
            <button type="button" disabled={readOnly} onClick={() => { const next = window.prompt("录音名称", selected.title); if (next?.trim()) void meetingUpdate(selected.id, { title: next.trim() }).then(record => { applyRecord(record); void refreshList(record.id, record.id); }).catch(error => onToast?.(errorText(error))); }}>重命名</button>
            <button type="button" disabled={readOnly || hasTranscriptDraft || needsTranscription || !selected.transcript.length} onClick={() => { if (!selected.minutes || window.confirm("重新生成会保留当前纪要和本地草稿，可在更多操作中恢复。继续生成？")) void regenerate(); }}>重新生成纪要</button>
            {selected.previousMinutes && <button type="button" disabled={readOnly || hasMinutesDraft} onClick={() => void meetingRestoreMinutes(selected.id).then(applyRecord).catch(error => onToast?.(errorText(error)))}>恢复上次纪要</button>}
            <button type="button" className="meeting-error-text" disabled={readOnly} onClick={() => void removeMeeting()}><Trash2 size={14} />删除录音</button>
          </div>}
          {!recordingThis && <nav className="meeting-tabs" aria-label="会议内容">
            <div role="tablist" aria-label="会议内容" onKeyDown={event => { if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return; event.preventDefault(); const next = event.key === "Home" ? "minutes" : event.key === "End" ? "transcript" : tab === "minutes" ? "transcript" : "minutes"; switchTab(next); event.currentTarget.querySelector<HTMLButtonElement>(`[data-tab="${next}"]`)?.focus(); }}><button type="button" role="tab" data-tab="minutes" id={`${tabPrefix}-minutes`} aria-controls={`${tabPrefix}-content`} aria-selected={tab === "minutes"} className={tab === "minutes" ? "is-active" : ""} onClick={() => switchTab("minutes")}>会议纪要</button><button type="button" role="tab" data-tab="transcript" id={`${tabPrefix}-transcript`} aria-controls={`${tabPrefix}-content`} aria-selected={tab === "transcript"} className={tab === "transcript" ? "is-active" : ""} onClick={() => switchTab("transcript")}>转写原文</button></div>
            <div className="meeting-tabs__actions">{tab === "transcript" && <label className="meeting-search"><Search size={14} /><input aria-label="查找原文" placeholder="查找原文" value={query} onChange={event => setQuery(event.target.value)} /></label>}
              {tab === "minutes" ? <button type="button" disabled={readOnly || hasTranscriptDraft || !!minutesConflict || !selected.minutes} onClick={() => setEditingMinutes(true)}><Pencil size={14} />编辑纪要</button> : <button type="button" disabled={readOnly || !!transcriptConflict || !selected.transcript.length} onClick={() => setEditingTranscript(true)}><Pencil size={14} />校对原文</button>}
            </div>
          </nav>}
          {recordingThis && recorder.transcription?.state === "unavailable" && <div className="meeting-workbench__notice" role="status">
            <span>{transcriptionIssue.message}</span>
            {transcriptionIssue.settings && onOpenModelSettings && <button type="button" onClick={onOpenModelSettings}>检查模型连接</button>}
            <button type="button" disabled={busy!==null||recorder.pendingSave} onClick={()=>void retryTranscription()}>{busy==='retry-transcription'?'正在重试…':'重试转写'}</button>
            {recorder.transcription.error && <details><summary>原因详情</summary><p>{recorder.transcription.error}</p></details>}
          </div>}
          {recordingThis && recorder.error && <div className="meeting-workbench__notice meeting-error-text" role="alert">{recorder.error}</div>}
          {processingThis && <div className="meeting-workbench__notice" role="status"><Loader2 size={15} className="meeting-spin" />{selected.status === "summarizing" ? "音频与原文已保存，正在整理纪要…" : "录音已保存，正在补齐转写并整理纪要…"}可以先查看原文或离开页面。</div>}
          {!recordingThis && !processingThis && (selected.error || needsTranscription) && <div className="meeting-workbench__notice" role="status"><span>{selected.status === "recorded" && !selected.error ? "录音已保存，尚有内容需要转写。" : "上次处理未完成，已保存的音频与文字仍可使用。"}</span><button type="button" disabled={busy !== null} onClick={() => void (!needsTranscription && selected.transcript.length ? regenerate() : processMeeting(selected))}>{selected.status === "recorded" && !selected.error ? "继续转写并整理纪要" : "重试未完成部分"}</button>{(!selectedModel || meetingTranscriptionIssue(selected.error).settings) && onOpenModelSettings && <button type="button" onClick={onOpenModelSettings}>检查模型连接</button>}{selected.error && <details><summary>错误详情</summary><p>{selected.error}</p></details>}</div>}
          {hasTranscriptDraft && tab === "minutes" && <div className="meeting-workbench__notice" role="status">原文校对尚未保存，请先保存校对并更新纪要。</div>}
          {selected.minutesStale && !processingThis && <div className="meeting-workbench__notice" role="status">当前纪要需要根据原文核对或更新。<button type="button" disabled={readOnly || hasTranscriptDraft || hasMinutesDraft || needsTranscription} onClick={() => void regenerate()}>更新纪要</button></div>}
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


          <div className="meeting-tab-content" id={`${tabPrefix}-content`} role={recordingThis ? "log" : "tabpanel"} aria-labelledby={recordingThis ? undefined : `${tabPrefix}-${tab}`} aria-live={recordingThis ? "polite" : undefined} aria-relevant="additions text" ref={contentRef} onWheel={() => setFollow(false)} onTouchMove={() => setFollow(false)} onPointerDown={event => { if (event.target === event.currentTarget) setFollow(false); }}>
            {tab === "minutes" && !recordingThis ? selected.minutes || editingMinutes ? <div className="meeting-minutes">
              {editingMinutes ? <div className="meeting-minutes__editor"><textarea aria-label="编辑会议纪要" rows={18} value={draftMinutes} disabled={readOnly || !!minutesConflict} onChange={event => { setDraftMinutes(event.target.value); meetingDrafts.editMinutes(selected, event.target.value); setHasMinutesDraft(meetingDrafts.hasMinutes(selected.id)); }} />
                <p className="meeting-muted" role="status">{meetingDrafts.persistenceError() ? "草稿尚未保存，请重试保存。" : "草稿自动保存在此设备，保存后更新正式纪要。"}</p><div className="meeting-edit-actions"><button type="button" className="meeting-primary" disabled={readOnly || !!minutesConflict || !draftMinutes.trim()} onClick={() => void saveMinutes()}>保存纪要</button><button type="button" onClick={() => setEditingMinutes(false)}>返回阅读（保留草稿）</button></div>
              </div> : <><Markdown complete markdownTheme="loose" config={{ onLinkClick: ({ href }) => { const match = /^#meeting-time-(\d+)$/.exec(href); if (match) { seekTo(Number(match[1]), false); return false; } } }}>{minutesMarkup}</Markdown>{hasMinutesDraft && <p className="meeting-muted">有尚未应用的纪要草稿。<button type="button" onClick={() => setEditingMinutes(true)}>继续编辑</button></p>}</>}
            </div> : <div className="meeting-workbench__empty"><Sparkles size={28} /><h3>{processingThis ? "正在整理会议纪要" : "尚未生成会议纪要"}</h3><p>{processingThis ? "原文和录音已保留，可以切换到转写原文查看。" : selected.transcript.length ? "根据已有原文整理摘要、结论与待办。" : "录音已保存，可以先回听，再重试转写。"}</p>{!processingThis && <button type="button" className="meeting-primary" disabled={readOnly || hasTranscriptDraft} onClick={() => void (!needsTranscription && selected.transcript.length ? regenerate() : processMeeting(selected))}>{selected.transcript.length ? "生成会议纪要" : "转写并生成纪要"}</button>}</div> : <div className="meeting-transcript">
              {editingTranscript && <div className="meeting-edit-actions"><span className="meeting-muted" role="status">{meetingDrafts.persistenceError() ? "草稿尚未保存" : "校对草稿已保存在此设备"}</span><button type="button" className="meeting-primary" disabled={readOnly || !!transcriptConflict} onClick={() => void saveTranscript()}>保存校对</button><button type="button" onClick={() => setEditingTranscript(false)}>返回阅读（保留草稿）</button></div>}
              {!editingTranscript && hasTranscriptDraft && <p className="meeting-muted">有尚未应用的原文校对草稿。<button type="button" disabled={readOnly} onClick={() => setEditingTranscript(true)}>继续校对</button></p>}
              {transcriptRows.map((segment, index) => <MeetingTranscriptPassage key={segment.id} segment={segment} index={index} active={activeSegment === segment.id && !recordingThis} editing={editingTranscript} disabled={readOnly || !!transcriptConflict} recording={recorder.active} register={registerRow} onSeek={seekTo} onText={changeText} onSpeaker={changeSpeaker} />)}
              {!transcriptRows.length && <div className="meeting-workbench__empty"><FileText size={28} /><h3>{query ? "没有匹配的原文" : recordingThis ? "正在聆听" : "尚无转写内容"}</h3><p>{recordingThis ? "开始说话后，文字会按短句出现在这里。" : "音频仍保存在本机，可以回听或重试转写。"}</p></div>}
              {recordingThis && !recorder.paused && <p className="meeting-transcript__pending" role="status">{recorder.transcription?.state === "recognizing" ? "正在识别最近一段语音…" : "等待下一段语音，文字按短句更新"}</p>}
            </div>}
          </div>
          {!follow && (recordingThis || playPosition >= 0 && tab === "transcript") && <button type="button" className="meeting-follow" onClick={() => setFollow(true)}><ArrowDown size={14} />{recordingThis ? "回到最新" : "跟随播放"}</button>}
          {recordingThis && <footer className="meeting-recording-controls" aria-label="录音控制">
            <div><strong>{formatMeetingDuration(recorder.meeting?.durationSeconds ?? 0)}</strong><meter aria-label="输入电平" min="0" max="1" value={recorder.level} /><span className="meeting-muted">{recorder.pendingSave ? "等待保存" : recorder.error ? "保存需要处理" : recorder.meeting?.recordedSamples ? "音频持续保存在本机" : "正在收音"}</span></div>
            <div>{!recorder.pendingSave && <button type="button" disabled={busy !== null || Boolean(recorder.error) && recorder.source !== "microphone"} onClick={() => void togglePause()}>{recorder.paused ? <Play size={15} /> : <Pause size={15} />}{recorder.paused ? "继续录音" : "暂停录音"}</button>}<button type="button" className="meeting-primary" disabled={busy !== null} onClick={() => void stopRecording()}>{busy === "stop" ? <Loader2 size={15} className="meeting-spin" /> : <CircleStop size={15} />}{recorder.pendingSave ? "重试保存" : "结束录音"}</button></div>
          </footer>}
          {!recorder.active && selected.recordedSamples > 0 && <MeetingAudioPlayer key={selected.id} control={playerControl} record={selected} request={seekRequest} onPosition={setPlayPosition} onToast={onToast} />}
        </>}
      </section>
    </div>
  </div>;
}
