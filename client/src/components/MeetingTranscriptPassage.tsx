import { memo } from "react";
import { formatMeetingDuration, type TranscriptSegment } from "@/lib/meeting-minutes";

/** Stable passages keep long transcripts cheap when the level meter ticks. */
export const MeetingTranscriptPassage = memo(function MeetingTranscriptPassage({ segment, index, active, editing, disabled, recording, register, onSeek, onText, onSpeaker }: {
  segment: TranscriptSegment;
  index: number;
  active: boolean;
  editing: boolean;
  disabled: boolean;
  recording: boolean;
  register: (id: number, node: HTMLDivElement | null) => void;
  onSeek: (time: number, autoplay: boolean) => void;
  onText: (id: number, text: string) => void;
  onSpeaker: (speaker: string, name: string) => void;
}) {
  return <div ref={node => register(segment.id, node)} className={`meeting-transcript__passage${active ? " is-playing" : ""}`}>
    <div className="meeting-transcript__metadata"><button type="button" disabled={recording} aria-label={`播放 ${formatMeetingDuration(segment.start)} 的原文`} onClick={() => onSeek(segment.start, true)}>{formatMeetingDuration(segment.start)}</button>
      {editing ? <input aria-label={`第 ${index + 1} 段说话人`} value={segment.speaker} disabled={disabled} onChange={event => onSpeaker(segment.speaker, event.target.value)} /> : <span>{segment.speaker || "发言片段"}</span>}
    </div>
    {editing ? <textarea aria-label={`第 ${index + 1} 段转写`} rows={Math.max(2, Math.ceil(segment.text.length / 50))} value={segment.text} disabled={disabled} onChange={event => onText(segment.id, event.target.value)} /> : <p>{segment.text}</p>}
  </div>;
});
