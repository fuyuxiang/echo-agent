import type { MeetingRecord, TranscriptSegment } from "./meeting-minutes";

export interface MeetingDraftConflict {
  original: string;
  canKeep: boolean;
}

interface Draft<T> {
  base: T;
  value: T;
  conflict?: MeetingDraftConflict;
}

const copySegments = (segments: TranscriptSegment[]) => segments.map((segment) => ({ ...segment }));
const same = (left: unknown, right: unknown) => JSON.stringify(left) === JSON.stringify(right);
const transcriptText = (segments: TranscriptSegment[]) => segments
  .map((segment) => `[${segment.start}–${segment.end}] ${segment.speaker}：${segment.text}`).join("\n");

// Segment IDs are renumbered when an earlier failed chunk is recovered.
// Cached ASR timing stays stable; occurrence counts disambiguate equal ranges.
function segmentKeys(segments: TranscriptSegment[]): string[] {
  const counts = new Map<string, number>();
  return segments.map(({ start, end }) => {
    const range = JSON.stringify([start, end]);
    const occurrence = counts.get(range) ?? 0;
    counts.set(range, occurrence + 1);
    return `${range}:${occurrence}`;
  });
}

export const MEETING_DRAFTS_KEY = "echo.meeting-drafts.v1";
type DraftStorage = Pick<Storage, "getItem" | "setItem">;

class DraftMap<T> extends Map<string, T> {
  constructor(private changed: () => void) { super(); }
  set(key: string, value: T) { super.set(key, value); this.changed(); return this; }
  delete(key: string) { const removed = super.delete(key); if (removed) this.changed(); return removed; }
  clear() { super.clear(); this.changed(); }
}

/** Device-local drafts survive restart and rebase against authoritative content. */
export function createMeetingDraftStore(storage?: DraftStorage) {
  let ready = false;
  let persistenceError: string | null = null;
  let unreadable = false;
  let originalBytes: string | null = null;
  let exitWithoutSaving = false;
  const snapshot = () => JSON.stringify({ version: 1, transcripts: [...transcripts], minutes: [...minutes] });
  const ensureDurable = () => {
    if (!storage || exitWithoutSaving) return;
    if (unreadable && originalBytes === null) {
      // A transient read error must be retryable. Validate recovered bytes and
      // keep an exact recovery copy before merging with newer in-memory edits.
      const recovered = storage.getItem(MEETING_DRAFTS_KEY);
      if (recovered) {
        const checked = createMeetingDraftStore({ getItem: () => recovered, setItem: () => {} });
        if (checked.persistenceError()) { originalBytes = recovered; throw new Error(checked.persistenceError()!); }
        const previousRecovery = storage.getItem(`${MEETING_DRAFTS_KEY}.recovery`);
        const recovery = previousRecovery && previousRecovery !== recovered
          ? JSON.stringify({ version: 1, recoveries: [previousRecovery, recovered] }) : recovered;
        storage.setItem(`${MEETING_DRAFTS_KEY}.recovery`, recovery);
        if (storage.getItem(`${MEETING_DRAFTS_KEY}.recovery`) !== recovery) throw new Error("恢复副本写入校验失败");
        const data = JSON.parse(recovered);
        for (const [key, value] of data.transcripts) if (!transcripts.has(key)) Map.prototype.set.call(transcripts, key, value);
        for (const [key, value] of data.minutes) if (!minutes.has(key)) Map.prototype.set.call(minutes, key, value);
      }
      unreadable = false;
    }
    if (unreadable) throw new Error(persistenceError ?? "会议草稿无法读取，请先恢复本地存储");
    try {
      const content = snapshot();
      storage.setItem(MEETING_DRAFTS_KEY, content);
      if (storage.getItem(MEETING_DRAFTS_KEY) !== content) throw new Error("写入校验失败");
      persistenceError = null;
    } catch (error) {
      persistenceError = `会议草稿尚未保存在此设备，请重试保存后再退出：${String(error)}`;
      throw new Error(persistenceError);
    }
  };
  const changed = () => { exitWithoutSaving = false; if (ready) { try { ensureDurable(); } catch { /* visible error + exit veto */ } } };
  type TranscriptDraft = Draft<TranscriptSegment[]> & { speakerRenames?: [string, string][] };
  const transcripts = new DraftMap<TranscriptDraft>(changed);
  const minutes = new DraftMap<Draft<string> & { transcriptBase: string }>(changed);
  if (storage) {
    try {
      const content = storage.getItem(MEETING_DRAFTS_KEY);
      originalBytes = content;
      if (content) {
        const data = JSON.parse(content);
        const validSegments = (value: unknown): value is TranscriptSegment[] => Array.isArray(value) && value.every(segment =>
          segment && typeof segment.text === "string" && typeof segment.speaker === "string" &&
          [segment.id, segment.start, segment.end].every(Number.isFinite));
        const validConflict = (conflict: unknown) => conflict === undefined || (!!conflict && typeof conflict === "object"
          && typeof (conflict as MeetingDraftConflict).original === "string" && typeof (conflict as MeetingDraftConflict).canKeep === "boolean");
        if (data?.version !== 1 || !Array.isArray(data.transcripts) || !Array.isArray(data.minutes)) throw new Error("草稿格式不受支持");
        for (const entry of data.transcripts) {
          if (!Array.isArray(entry) || typeof entry[0] !== "string" || !validSegments(entry[1]?.base)
            || !validSegments(entry[1]?.value) || !validConflict(entry[1]?.conflict)) throw new Error("校对草稿格式损坏");
          const renames = entry[1].speakerRenames;
          if (renames !== undefined && (!Array.isArray(renames) || !renames.every(pair =>
            Array.isArray(pair) && pair.length === 2 && pair.every(name => typeof name === "string")))) {
            throw new Error("说话人更名草稿格式损坏");
          }
          transcripts.set(entry[0], entry[1]);
        }
        for (const entry of data.minutes) {
          if (!Array.isArray(entry) || typeof entry[0] !== "string" || typeof entry[1]?.base !== "string"
            || typeof entry[1]?.value !== "string" || typeof entry[1]?.transcriptBase !== "string"
            || !validConflict(entry[1]?.conflict)) throw new Error("纪要草稿格式损坏");
          minutes.set(entry[0], entry[1]);
        }
      }
    } catch (error) {
      // Preserve the original bytes. Never replace unreadable drafts with an
      // empty map; the UI and exit guard surface the recovery requirement.
      unreadable = true;
      persistenceError = `本地会议草稿读取失败，原数据已保留：${String(error)}`;
    }
  }
  ready = true;

  const transcript = (record: MeetingRecord): TranscriptDraft => {
    const draft = transcripts.get(record.id);
    if (!draft) return { base: record.transcript, value: record.transcript };
    // Processing clears the manifest before the new complete/partial result.
    if (record.status === "transcribing" && record.transcript.length === 0) return draft;
    if (same(draft.base, record.transcript)) return draft;

    const baseKeys = segmentKeys(draft.base);
    const valueKeys = segmentKeys(draft.value);
    const incomingKeys = segmentKeys(record.transcript);
    const edited = new Map(valueKeys.map((key, index) => [key, draft.value[index]]));
    const incoming = new Map(incomingKeys.map((key, index) => [key, index]));
    const merged = copySegments(record.transcript);
    let conflicting = false;
    let missing = false;
    draft.base.forEach((base, index) => {
      const local = edited.get(baseKeys[index]);
      if (!local) return;
      const fields = (["text", "speaker"] as const).filter((field) => local[field] !== base[field]);
      if (!fields.length) return;
      const target = incoming.get(baseKeys[index]);
      if (target === undefined) { missing = true; return; }
      for (const field of fields) {
        const latest = merged[target][field];
        if (latest !== base[field] && latest !== local[field]) conflicting = true;
        merged[target][field] = local[field];
      }
    });
    const conflict = draft.conflict || conflicting || missing ? {
      original: draft.conflict?.original ?? transcriptText(draft.value),
      canKeep: !missing && (draft.conflict?.canKeep ?? true),
    } : undefined;
    const next = { ...draft, base: copySegments(record.transcript), value: merged, conflict };
    if (!conflict && same(merged, record.transcript)) transcripts.delete(record.id);
    else transcripts.set(record.id, next);
    return next;
  };

  const minute = (record: MeetingRecord): Draft<string> => {
    const draft = minutes.get(record.id);
    const latest = record.minutes ?? "";
    if (!draft) return { base: latest, value: latest };
    if (draft.base === latest && draft.transcriptBase === JSON.stringify(record.transcript)) return draft;
    if (draft.value === latest && !draft.conflict) {
      minutes.delete(record.id);
      return { base: latest, value: latest };
    }
    const next = { ...draft, base: latest, transcriptBase: JSON.stringify(record.transcript),
      conflict: draft.conflict ?? { original: draft.value, canKeep: true } };
    minutes.set(record.id, next);
    return next;
  };

  return {
    ensureDurable,
    allowExitWithoutSaving: () => { exitWithoutSaving = true; },
    cancelExitDiscard: () => { exitWithoutSaving = false; },
    recoveryBytes: () => unreadable ? originalBytes : null,
    resetUnreadable() {
      if (!unreadable || !storage) return;
      // Retain an exportable copy before resetting; failed writes leave the
      // original key untouched and continue to veto automatic exit.
      storage.setItem(`${MEETING_DRAFTS_KEY}.recovery`, originalBytes ?? "");
      storage.setItem(MEETING_DRAFTS_KEY, JSON.stringify({ version: 1, transcripts: [], minutes: [] }));
      unreadable = false;
      transcripts.clear(); minutes.clear();
      persistenceError = null;
    },
    persistenceError: () => persistenceError,
    transcript,
    minutes: minute,
    editTranscript(record: MeetingRecord, value: TranscriptSegment[], rename?: [string, string]) {
      const draft = transcript(record);
      if (!draft.conflict && same(value, record.transcript)) transcripts.delete(record.id);
      else {
        const speakerRenames = rename
          ? [...(draft.speakerRenames ?? []).filter(([from]) => from !== rename[0]), rename]
          : draft.speakerRenames;
        transcripts.set(record.id, { ...draft, base: copySegments(draft.base), value: copySegments(value), speakerRenames });
      }
    },
    editMinutes(record: MeetingRecord, value: string) {
      const draft = minute(record);
      if (!draft.conflict && value === (record.minutes ?? "")) minutes.delete(record.id);
      else minutes.set(record.id, { ...draft, value, transcriptBase: JSON.stringify(record.transcript) });
    },
    acceptTranscript(record: MeetingRecord) {
      const draft = transcript(record);
      if (draft.conflict?.canKeep === false) return;
      transcripts.set(record.id, { ...draft, base: copySegments(record.transcript), value: copySegments(draft.value), conflict: undefined });
    },
    acceptMinutes(record: MeetingRecord) {
      const draft = minute(record);
      minutes.set(record.id, { base: record.minutes ?? "", value: draft.value,
        transcriptBase: JSON.stringify(record.transcript) });
    },
    savedTranscript(record: MeetingRecord, submitted: TranscriptSegment[]) {
      const draft = transcripts.get(record.id);
      if (!draft || same(draft.value, submitted)) transcripts.delete(record.id);
      else transcripts.set(record.id, { ...draft, base: copySegments(record.transcript) });
    },
    savedMinutes(record: MeetingRecord, submitted: string) {
      const draft = minutes.get(record.id);
      if (!draft || draft.value === submitted) minutes.delete(record.id);
      else minutes.set(record.id, { ...draft, base: record.minutes ?? "", transcriptBase: JSON.stringify(record.transcript) });
    },
    hasMinutes: (id: string) => minutes.has(id),
    hasTranscript: (id: string) => transcripts.has(id),
    discardTranscript: (id: string) => { transcripts.delete(id); },
    discardMinutes: (id: string) => { minutes.delete(id); },
    remove(id: string) { transcripts.delete(id); minutes.delete(id); },
    clear() { transcripts.clear(); minutes.clear(); },
  };
}

function deviceDraftStorage(): DraftStorage {
  return {
    getItem: key => globalThis.localStorage.getItem(key),
    setItem: (key, value) => globalThis.localStorage.setItem(key, value),
  };
}
export const meetingDrafts = createMeetingDraftStore(deviceDraftStorage());
