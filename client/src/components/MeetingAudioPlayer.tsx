import { useEffect, useImperativeHandle, useRef, useState, type MutableRefObject } from "react";
import { ExternalLink, Pause, Play, RotateCcw, RotateCw, Volume2 } from "lucide-react";
import { formatMeetingDuration, meetingAudioUrl, meetingOpenAudio, type MeetingRecord } from "@/lib/meeting-minutes";

export interface MeetingSeek { time: number; autoplay: boolean; sequence: number }
export interface MeetingPlayerHandle { seek: (request: MeetingSeek) => void }
const positions = new Map<string, number>();

export function MeetingAudioPlayer({ record, request, control, onPosition, onToast }: {
  record: MeetingRecord;
  request?: MeetingSeek;
  control?: MutableRefObject<MeetingPlayerHandle | null>;
  onPosition: (seconds: number) => void;
  onToast?: (message: string) => void;
}) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const lastAudio = useRef<HTMLAudioElement | null>(null);
  const pending = useRef<MeetingSeek>();
  const handled = useRef(-1);
  const [url, setUrl] = useState("");
  const [failure, setFailure] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [ready, setReady] = useState(false);
  const [position, setPosition] = useState(positions.get(record.id) ?? 0);
  const [duration, setDuration] = useState(record.durationSeconds);
  const [speed, setSpeed] = useState(1);

  useEffect(() => {
    let canceled = false;
    setFailure(""); setUrl(""); setReady(false); setPlaying(false);
    void meetingAudioUrl(record.id).then(value => { if (!canceled) setUrl(value); })
      .catch(() => { if (!canceled) setFailure("暂时无法读取录音，文字内容仍可使用。"); });
    return () => { canceled = true; lastAudio.current?.pause(); };
  }, [record.id, attempt]);

  const seek = (seconds: number, autoplay = false) => {
    const audio = audioRef.current;
    if (!audio) return;
    const target = Math.max(0, Math.min(seconds, duration || record.durationSeconds));
    audio.currentTime = target;
    setPosition(target); onPosition(target); positions.set(record.id, target);
    if (autoplay) void audio.play().catch(error => { if (error?.name !== "AbortError") onToast?.("暂时无法播放，请点击播放按钮重试"); });
    else audio.pause();
  };

  const applyPending = () => {
    if (pending.current && audioRef.current && audioRef.current.readyState >= 1) {
      const next = pending.current; pending.current = undefined; handled.current = next.sequence;
      seek(next.time, next.autoplay);
    }
  };
  // Seek/play directly in the timestamp click's user gesture (WKWebView).
  // Metadata not yet available remains a deferred request.
  useImperativeHandle(control, () => ({ seek(next) { pending.current = next; applyPending(); } }));
  useEffect(() => {
    if (!request || handled.current === request.sequence) return;
    pending.current = request;
    applyPending();
  // Request is an imperative seek, independent from normal playback ticks.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [request, url]);

  if (failure) return <footer className="meeting-player meeting-player--error" aria-label="录音播放器">
    <span role="status">{failure}</span>
    <button type="button" onClick={() => setAttempt(value => value + 1)}>重试播放</button>
    <button type="button" onClick={() => void meetingOpenAudio(record.id).catch(() => onToast?.("无法打开录音，请检查文件是否仍在本机"))}><ExternalLink size={14} />系统播放器</button>
  </footer>;

  return <footer className="meeting-player" aria-label="录音播放器">
    <audio ref={node => { audioRef.current = node; if (node) lastAudio.current = node; }} src={url || undefined} preload="metadata"
      onLoadedMetadata={() => {
        const audio = audioRef.current!;
        if (Number.isFinite(audio.duration)) setDuration(audio.duration);
        setReady(true);
        audio.currentTime = positions.get(record.id) ?? 0;
        audio.playbackRate = speed; applyPending();
      }}
      onCanPlay={applyPending}
      onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)}
      onTimeUpdate={() => { const time = audioRef.current?.currentTime ?? 0; setPosition(time); positions.set(record.id, time); if (positions.size > 100) positions.delete(positions.keys().next().value!); onPosition(time); }}
      onError={() => { if (url) setFailure("录音暂时无法播放，文字内容仍可使用。"); }} />
    <div className="meeting-player__controls">
      <button className="meeting-player__play" type="button" disabled={!url || !ready} aria-label={playing ? "暂停播放" : "播放录音"} onClick={() => {
        const audio = audioRef.current;
        if (!audio) return;
        if (audio.paused) void audio.play().catch(error => { if (error?.name !== "AbortError") onToast?.("播放未开始，请重试"); }); else audio.pause();
      }}>{playing ? <Pause size={17} /> : <Play size={17} />}</button>
      <button type="button" disabled={!ready} aria-label="后退 10 秒" onClick={() => seek(position - 10, playing)}><RotateCcw size={15} /></button>
      <span className="meeting-player__time">{formatMeetingDuration(position)} / {formatMeetingDuration(duration)}</span>
      <button type="button" disabled={!ready} aria-label="前进 10 秒" onClick={() => seek(position + 10, playing)}><RotateCw size={15} /></button>
      <select aria-label="播放速度" value={speed} onChange={event => { const rate = Number(event.target.value); setSpeed(rate); if (audioRef.current) audioRef.current.playbackRate = rate; }}>
        {[.75, 1, 1.25, 1.5, 2].map(rate => <option key={rate} value={rate}>{rate}×</option>)}
      </select>
      <details className="meeting-player__volume"><summary aria-label="音量"><Volume2 size={15} /></summary><input aria-label="音量" type="range" min="0" max="1" step="0.05" defaultValue="1" onChange={event => { if (audioRef.current) audioRef.current.volume = Number(event.target.value); }} /></details>
    </div>
    <input className="meeting-player__progress" aria-label="播放进度" type="range" min="0" max={duration || 1} step="0.1" value={Math.min(position, duration || 1)} disabled={!url || !ready} onChange={event => seek(Number(event.target.value), playing)} />
  </footer>;
}
