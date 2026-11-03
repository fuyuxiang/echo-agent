import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  registerTtsProvider,
  registerAsrProvider,
  resetVoiceRegistry,
  getActiveTts,
  getActiveAsr,
  listProviderIds,
  createWebSpeechAsrProvider,
  createWebSpeechTtsProvider,
  nextTtsStatus,
  nextAsrStatus,
  type TtsProvider,
  type AsrProvider,
  type SpeechRecognitionLike,
} from "../voice-contract";

describe("voice-contract — 注册表", () => {
  beforeEach(resetVoiceRegistry);

  const tts = (id: string, avail = true): TtsProvider => ({
    id,
    isAvailable: () => avail,
    speak: () => () => {},
  });
  const asr = (id: string, avail = true): AsrProvider => ({
    id,
    isAvailable: () => avail,
    listen: () => () => {},
  });

  it("注册后可列出 id", () => {
    registerTtsProvider(tts("cloud-tts"));
    registerAsrProvider(asr("cloud-asr"));
    expect(listProviderIds()).toEqual({ tts: ["cloud-tts"], asr: ["cloud-asr"] });
  });

  it("getActive 取第一个可用 provider", () => {
    registerTtsProvider(tts("unavailable", false));
    registerTtsProvider(tts("available"));
    expect(getActiveTts()?.id).toBe("available");
  });

  it("无可用 provider 返回 null", () => {
    registerAsrProvider(asr("x", false));
    expect(getActiveAsr()).toBeNull();
    expect(getActiveTts()).toBeNull();
  });

  it("重复注册同 id 覆盖且提到最高优先级", () => {
    registerTtsProvider(tts("a"));
    registerTtsProvider(tts("b"));
    registerTtsProvider(tts("a")); // 重新注册 a
    const ids = listProviderIds().tts;
    expect(ids[0]).toBe("a"); // a 提到最前
    expect(ids).toHaveLength(2); // 不重复
  });

  it("reset 清空注册表", () => {
    registerTtsProvider(tts("a"));
    resetVoiceRegistry();
    expect(listProviderIds()).toEqual({ tts: [], asr: [] });
  });
});

describe("createWebSpeechAsrProvider", () => {
  beforeEach(() => { resetVoiceRegistry(); vi.useFakeTimers(); });
  afterEach(() => vi.useRealTimers());

  function setup() {
    const recognitions: SpeechRecognitionLike[] = [];
    const provider = createWebSpeechAsrProvider({
      isAvailable: () => true,
      createRecognition: () => {
        const rec: SpeechRecognitionLike = {
          lang: "", continuous: false, interimResults: false,
          start: vi.fn(), stop: vi.fn(), abort: vi.fn(),
          onresult: null, onerror: null, onend: null,
        };
        recognitions.push(rec);
        return rec;
      },
    });
    const handlers = { onInterim: vi.fn(), onFinal: vi.fn(), onEnd: vi.fn(), onError: vi.fn() };
    const stop = provider.listen("zh-CN", handlers);
    return { recognitions, handlers, stop };
  }
  const finalResult = (text: string) => ({ resultIndex: 0, results: [{ isFinal: true, 0: { transcript: text } }] });

  it("listen 把中间/最终结果回调出去,并返回 stop", () => {
    let started = false;
    const fakeRec = {
      lang: "",
      interimResults: false,
      continuous: false,
      start: () => {
        started = true;
      },
      stop: vi.fn(),
      onresult: null as ((e: unknown) => void) | null,
      onerror: null as ((e: unknown) => void) | null,
      onend: null as (() => void) | null,
    };
    const provider = createWebSpeechAsrProvider({
      isAvailable: () => true,
      createRecognition: (lang) => {
        fakeRec.lang = lang;
        return fakeRec;
      },
    });
    const onInterim = vi.fn();
    const onFinal = vi.fn();
    const onEnd = vi.fn();
    const stop = provider.listen("zh-CN", { onInterim, onFinal, onEnd });
    expect(started).toBe(true);
    expect(fakeRec.lang).toBe("zh-CN");

    // 模拟一条中间 + 一条最终结果。
    fakeRec.onresult!({
      resultIndex: 0,
      results: [
        { isFinal: false, 0: { transcript: "你" } },
        { isFinal: true, 0: { transcript: "你好" } },
      ],
    });
    expect(onInterim).toHaveBeenCalledWith("你");
    expect(onFinal).toHaveBeenCalledWith("你好");

    // Later result events report only their newly-finalized segment. This is
    // what lets the Composer append without duplicating the earlier text.
    fakeRec.onresult!({
      resultIndex: 0,
      results: [{ isFinal: true, 0: { transcript: "世界" } }],
    });
    expect(onFinal).toHaveBeenLastCalledWith("世界");

    stop();
    expect(fakeRec.stop).toHaveBeenCalled();
    fakeRec.onend!();
    expect(onEnd).toHaveBeenCalledOnce();
  });

  it("uses continuous dictation and stays active across an engine session boundary", () => {
    const { recognitions, handlers, stop } = setup();
    const first = recognitions[0];
    expect(first).toMatchObject({ lang: "zh-CN", continuous: true, interimResults: true });
    first.onresult?.(finalResult("第一句。"));
    first.onend?.();
    expect(handlers.onEnd).not.toHaveBeenCalled();
    vi.advanceTimersByTime(250);
    recognitions[1].onresult?.(finalResult("第二句。"));
    expect(handlers.onFinal.mock.calls).toEqual([["第一句。"], ["第二句。"]]);
    expect(handlers.onEnd).not.toHaveBeenCalled();
    stop.abort?.();
  });

  it("recovers from no-speech without ending dictation or reporting a transient error", () => {
    const { recognitions, handlers, stop } = setup();
    recognitions[0].onerror?.({ error: "no-speech" });
    recognitions[0].onend?.();
    vi.advanceTimersByTime(250);
    expect(recognitions).toHaveLength(2);
    expect(handlers.onError).not.toHaveBeenCalled();
    expect(handlers.onEnd).not.toHaveBeenCalled();
    recognitions[1].onresult?.(finalResult("停顿后的第二句"));
    expect(handlers.onFinal).toHaveBeenCalledWith("停顿后的第二句");
    stop.abort?.();
  });

  it("stopping between sessions cancels a pending restart", () => {
    const { recognitions, handlers, stop } = setup();
    recognitions[0].onend?.();
    stop();
    stop();
    vi.runAllTimers();
    expect(recognitions).toHaveLength(1);
    expect(handlers.onEnd).toHaveBeenCalledOnce();
  });

  it("explicit stop accepts the last finalized words and never restarts", () => {
    const { recognitions, handlers, stop } = setup();
    const rec = recognitions[0];
    stop(); stop();
    expect(rec.stop).toHaveBeenCalledOnce();
    rec.onresult?.(finalResult("最后一句"));
    rec.onend?.();
    vi.runAllTimers();
    expect(handlers.onFinal).toHaveBeenCalledWith("最后一句");
    expect(handlers.onEnd).toHaveBeenCalledOnce();
    expect(recognitions).toHaveLength(1);
  });

  it("ignores repeated final snapshots and callbacks from a disconnected session", () => {
    const { recognitions, handlers, stop } = setup();
    const first = recognitions[0];
    const lateResult = first.onresult!;
    const lateEnd = first.onend!;
    first.onresult?.(finalResult("第一句"));
    first.onresult?.(finalResult("第一句"));
    first.onend?.();
    vi.advanceTimersByTime(250);
    lateResult(finalResult("迟到结果")); lateEnd();
    expect(handlers.onFinal.mock.calls).toEqual([["第一句"]]);
    expect(recognitions).toHaveLength(2);
    stop.abort?.();
  });

  it("keeps an unfinished interim when the engine disconnects before finalizing it", () => {
    const { recognitions, handlers, stop } = setup();
    recognitions[0].onresult?.({ resultIndex: 0, results: [{ isFinal: false, 0: { transcript: "尚未定稿的句子" } }] });
    recognitions[0].onend?.();
    expect(handlers.onFinal).toHaveBeenCalledWith("尚未定稿的句子");
    stop.abort?.();
    vi.runAllTimers();
    expect(recognitions).toHaveLength(1);
  });

  it.each(["not-allowed", "service-not-allowed", "audio-capture", "network", "aborted"])("stops on terminal error %s without retrying", error => {
    const { recognitions, handlers } = setup();
    const rec = recognitions[0];
    const lateEnd = rec.onend!;
    rec.onerror?.({ error }); lateEnd();
    vi.runAllTimers();
    expect(rec.abort).toHaveBeenCalledOnce();
    expect(handlers.onError).toHaveBeenCalledExactlyOnceWith(error);
    expect(handlers.onEnd).toHaveBeenCalledOnce();
    expect(recognitions).toHaveLength(1);
  });

  it("bounds consecutive empty sessions instead of looping forever", () => {
    const { recognitions, handlers } = setup();
    for (let i = 0; i < 10 && !handlers.onEnd.mock.calls.length; i++) {
      const rec = recognitions[recognitions.length - 1];
      rec.onerror?.({ error: "no-speech" });
      rec.onend?.();
      vi.runAllTimers();
    }
    expect(recognitions.length).toBeGreaterThan(1);
    expect(recognitions.length).toBeLessThan(10);
    expect(handlers.onError).toHaveBeenCalledExactlyOnceWith("no-speech");
    expect(handlers.onEnd).toHaveBeenCalledOnce();
  });

  it("abort releases the microphone and ignores all late callbacks", () => {
    const { recognitions, handlers, stop } = setup();
    const rec = recognitions[0];
    const lateResult = rec.onresult!;
    stop.abort?.(); stop.abort?.();
    lateResult(finalResult("取消后的迟到结果"));
    vi.runAllTimers();
    expect(rec.abort).toHaveBeenCalledOnce();
    expect(handlers.onFinal).not.toHaveBeenCalled();
    expect(handlers.onEnd).toHaveBeenCalledOnce();
    expect(recognitions).toHaveLength(1);
  });

  it("start 抛错时回调 onError", () => {
    const provider = createWebSpeechAsrProvider({
      isAvailable: () => true,
      createRecognition: () => ({
        lang: "",
        interimResults: false,
        continuous: false,
        start: () => {
          throw new Error("boom");
        },
        stop: () => {},
        onresult: null,
        onerror: null,
        onend: null,
      }),
    });
    const onError = vi.fn();
    const onEnd = vi.fn();
    provider.listen("en", { onError, onEnd });
    expect(onError).toHaveBeenCalledWith("start-failed");
    expect(onEnd).toHaveBeenCalledOnce();
  });

  it("isAvailable 透传依赖谓词", () => {
    const p = createWebSpeechAsrProvider({
      isAvailable: () => false,
      createRecognition: () => ({}) as never,
    });
    expect(p.isAvailable()).toBe(false);
  });
});

describe("createWebSpeechTtsProvider", () => {
  beforeEach(resetVoiceRegistry);

  it("speak 调用 synth.speak 并返回 stop(=cancel)", () => {
    const synth = { cancel: vi.fn(), speak: vi.fn() };
    const provider = createWebSpeechTtsProvider({
      isAvailable: () => true,
      createUtterance: (_text, lang, opts) => ({ lang, ...opts }),
      synth,
    });
    const stop = provider.speak("你好", "zh-CN", { rate: 1 });
    expect(synth.cancel).toHaveBeenCalled();
    expect(synth.speak).toHaveBeenCalled();
    stop();
    // stop 至少再 cancel 一次。
    expect(synth.cancel.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("空文本不强制要求 speak(由调用方决定),但 provider 可被调用", () => {
    const synth = { cancel: vi.fn(), speak: vi.fn() };
    const provider = createWebSpeechTtsProvider({
      isAvailable: () => true,
      createUtterance: () => ({ lang: "zh-CN" }),
      synth,
    });
    provider.speak("", "zh-CN");
    expect(synth.speak).toHaveBeenCalled();
  });
});

describe("nextTtsStatus / nextAsrStatus 状态机", () => {
  it("TTS: idle → speaking → idle", () => {
    expect(nextTtsStatus("idle", "start")).toBe("speaking");
    expect(nextTtsStatus("speaking", "stop")).toBe("idle");
  });
  it("TTS: speaking → paused → speaking", () => {
    expect(nextTtsStatus("speaking", "pause")).toBe("paused");
    expect(nextTtsStatus("paused", "resume")).toBe("speaking");
    expect(nextTtsStatus("paused", "start")).toBe("speaking");
  });
  it("TTS: 非法迁移保持原状", () => {
    expect(nextTtsStatus("idle", "pause")).toBe("idle");
    expect(nextTtsStatus("idle", "resume")).toBe("idle");
    expect(nextTtsStatus("speaking", "start")).toBe("speaking");
  });
  it("TTS: fail → error", () => {
    expect(nextTtsStatus("speaking", "fail")).toBe("error");
  });

  it("ASR: idle → listening → idle", () => {
    expect(nextAsrStatus("idle", "start")).toBe("listening");
    expect(nextAsrStatus("listening", "stop")).toBe("idle");
  });
  it("ASR: 非法迁移保持原状", () => {
    expect(nextAsrStatus("listening", "start")).toBe("listening");
    expect(nextAsrStatus("idle", "stop")).toBe("idle");
  });
  it("ASR: fail → error", () => {
    expect(nextAsrStatus("listening", "fail")).toBe("error");
  });
});
