/**
 * 语音能力 provider-agnostic 契约 —— 对齐 EchoAgent `tts:*` / `asr:*` RPC
 * (`tts:startTts` / `tts:stopTts` / `tts:event` / `asr:speechToText`)。
 *
 * EchoAgent 的 TTS/ASR 契约本身 provider 无关(可绑定任意 STT/TTS 厂商);
 * EchoAgent 是 BYOK 桌面应用,默认用浏览器 Web Speech API 作为内建 provider,
 * 同时允许注册外部 provider(例如对接云端 STT/TTS)。本模块是纯注册表 + 状态机,
 * 无 DOM/网络副作用,便于单测。
 */

/** 一次 TTS 任务的状态。 */
export type TtsStatus = "idle" | "speaking" | "paused" | "error";

/** ASR 识别状态。 */
export type AsrStatus = "idle" | "listening" | "error";

/** TTS provider 接口(任意实现:Web Speech、云端 TTS、本地模型)。 */
export interface TtsProvider {
  id: string;
  /** 开始朗读;text 非空才会调用。返回一个 stop 函数。 */
  speak(text: string, lang: string, opts?: {
    rate?: number;
    pitch?: number;
    onEnd?: () => void;
    onError?: () => void;
  }): () => void;
  /** 是否可用(env 不支持时返回 false)。 */
  isAvailable(): boolean;
}

/** ASR provider 接口。 */
export type AsrStop = (() => void) & { abort?: () => void };

export interface AsrProvider {
  id: string;
  /**
   * 开始监听;通过回调实时回报:
   *  - onInterim:中间结果(实时回显用)
   *  - onFinal:最终结果
   *  - onError:出错
   *  - onEnd:会话结束(无论正常/异常)
   * 返回一个 stop 函数(结束听写，允许收齐最后的识别结果)。
   * 可选的 stop.abort 用于离开输入框时立即取消，不再返回结果。
   */
  listen(
    lang: string,
    handlers: {
      onInterim?: (text: string) => void;
      onFinal?: (text: string) => void;
      onError?: (reason: string) => void;
      onEnd?: () => void;
    },
  ): AsrStop;
  /** 是否可用。 */
  isAvailable(): boolean;
}

interface VoiceRegistry {
  tts: TtsProvider[];
  asr: AsrProvider[];
}

const registry: VoiceRegistry = { tts: [], asr: [] };

/** 注册一个 TTS provider(后注册的优先级更高 = 排在前面)。 */
export function registerTtsProvider(p: TtsProvider): void {
  registry.tts = [p, ...registry.tts.filter((x) => x.id !== p.id)];
}

/** 注册一个 ASR provider。 */
export function registerAsrProvider(p: AsrProvider): void {
  registry.asr = [p, ...registry.asr.filter((x) => x.id !== p.id)];
}

/** 清空所有 provider(测试用)。 */
export function resetVoiceRegistry(): void {
  registry.tts = [];
  registry.asr = [];
}

/** 取第一个可用的 TTS provider(优先级最高);无则 null。 */
export function getActiveTts(): TtsProvider | null {
  return registry.tts.find((p) => p.isAvailable()) ?? null;
}

/** 取第一个可用的 ASR provider;无则 null。 */
export function getActiveAsr(): AsrProvider | null {
  return registry.asr.find((p) => p.isAvailable()) ?? null;
}

/** 列出所有已注册 provider 的 id(调试/状态展示用)。 */
export function listProviderIds(): { tts: string[]; asr: string[] } {
  return {
    tts: registry.tts.map((p) => p.id),
    asr: registry.asr.map((p) => p.id),
  };
}

// ---------- 内建 Web Speech provider(默认,可被外部 provider 覆盖)----------

/** 最小 SpeechRecognition 构造器形状(避免耦合 DOM 类型)。 */
export interface SpeechRecognitionLike {
  lang: string;
  interimResults: boolean;
  continuous: boolean;
  start(): void;
  stop(): void;
  abort?: () => void;
  onresult: ((e: SpeechRecognitionEventLike) => void) | null;
  onerror: ((e: SpeechRecognitionErrorEventLike) => void) | null;
  onend: (() => void) | null;
}
interface SpeechRecognitionEventLike {
  resultIndex: number;
  results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }>;
}
interface SpeechRecognitionErrorEventLike {
  error: string;
}
interface SpeechSynthesisLike {
  cancel(): void;
  speak(utter: { rate?: number; pitch?: number; lang?: string; onend?: () => void; onerror?: () => void }): void;
}

/**
 * 创建内建 Web Speech ASR provider。传入依赖注入以保持可测(env 谓词 + 构造器)。
 */
export function createWebSpeechAsrProvider(deps: {
  isAvailable: () => boolean;
  createRecognition: (lang: string) => SpeechRecognitionLike;
}): AsrProvider {
  return {
    id: "web-speech-asr",
    isAvailable: deps.isAvailable,
    listen(lang, handlers) {
      let recognition: SpeechRecognitionLike | null = null;
      let restartTimer: ReturnType<typeof setTimeout> | undefined;
      let listening = true;
      let ended = false;
      let emptyRounds = 0;
      const detach = (rec: SpeechRecognitionLike) => {
        rec.onresult = rec.onerror = rec.onend = null;
      };
      const finish = (reason?: string) => {
        if (ended) return;
        ended = true;
        listening = false;
        clearTimeout(restartTimer);
        const rec = recognition;
        recognition = null;
        if (rec) {
          detach(rec);
          try { if (rec.abort) rec.abort(); else rec.stop(); } catch { /* already stopped */ }
        }
        if (reason) handlers.onError?.(reason);
        handlers.onEnd?.();
      };
      const startRound = () => {
        if (!listening || ended) return;
        try {
          const rec = deps.createRecognition(lang);
          recognition = rec;
          rec.lang = lang;
          rec.continuous = true;
          rec.interimResults = true;
          const finalized = new Set<number>();
          let interim = "";
          let hasSpeech = false;
          rec.onresult = (event) => {
            if (ended || recognition !== rec) return;
            interim = "";
            let finalDelta = "";
            for (let i = 0; i < event.results.length; i++) {
              const result = event.results[i];
              if (!result.isFinal) interim += result[0].transcript;
              else if (!finalized.has(i)) {
                finalized.add(i);
                finalDelta += result[0].transcript;
              }
            }
            if (interim || finalDelta) { hasSpeech = true; emptyRounds = 0; }
            if (finalDelta) handlers.onFinal?.(finalDelta);
            if (!ended && recognition === rec) handlers.onInterim?.(interim);
          };
          rec.onerror = (event) => {
            if (ended || recognition !== rec) return;
            // Silence is an engine session boundary, not the user's stop
            // command. Permission, device and network errors are terminal.
            if (event.error !== "no-speech") finish(event.error);
          };
          rec.onend = () => {
            if (ended || recognition !== rec) return;
            detach(rec);
            recognition = null;
            // Some engines disconnect before finalizing their last interim.
            // Keep that visible text when continuing into the next session.
            if (interim) handlers.onFinal?.(interim);
            if (!listening) { finish(); return; }
            if (!hasSpeech && ++emptyRounds > 3) { finish("no-speech"); return; }
            // Continuous mode can still end at an engine-imposed boundary.
            // Restart without ending the logical dictation or resetting UI.
            restartTimer = setTimeout(startRound, 250);
          };
          rec.start();
        } catch {
          finish("start-failed");
        }
      };
      startRound();
      const stop: AsrStop = () => {
        if (!listening || ended) return;
        listening = false;
        clearTimeout(restartTimer);
        if (!recognition) { finish(); return; }
        try { recognition.stop(); } catch { finish(); }
      };
      stop.abort = () => finish();
      return stop;
    },
  };
}

/**
 * 创建内建 Web Speech TTS provider(依赖注入以保持可测)。
 */
export function createWebSpeechTtsProvider(deps: {
  isAvailable: () => boolean;
  createUtterance: (text: string, lang: string, opts?: {
    rate?: number;
    pitch?: number;
    onEnd?: () => void;
    onError?: () => void;
  }) => {
    rate?: number;
    pitch?: number;
    lang?: string;
    onend?: () => void;
    onerror?: () => void;
  };
  synth: SpeechSynthesisLike;
}): TtsProvider {
  return {
    id: "web-speech-tts",
    isAvailable: deps.isAvailable,
    speak(text, lang, opts) {
      const u = deps.createUtterance(text, lang, opts);
      deps.synth.cancel();
      deps.synth.speak(u);
      return () => deps.synth.cancel();
    },
  };
}

// ---------- 运行时状态机(可选,供 UI 反馈)----------

/** TTS 状态机:从 idle 出发,限定合法迁移。 */
export function nextTtsStatus(from: TtsStatus, action: "start" | "stop" | "pause" | "resume" | "fail"): TtsStatus {
  switch (action) {
    case "start":
      return from === "idle" || from === "paused" ? "speaking" : from;
    case "stop":
      return from === "speaking" || from === "paused" ? "idle" : from;
    case "pause":
      return from === "speaking" ? "paused" : from;
    case "resume":
      return from === "paused" ? "speaking" : from;
    case "fail":
      return "error";
  }
}

/** ASR 状态机。 */
export function nextAsrStatus(from: AsrStatus, action: "start" | "stop" | "fail"): AsrStatus {
  switch (action) {
    case "start":
      return from === "idle" ? "listening" : from;
    case "stop":
      return from === "listening" ? "idle" : from;
    case "fail":
      return "error";
  }
}
