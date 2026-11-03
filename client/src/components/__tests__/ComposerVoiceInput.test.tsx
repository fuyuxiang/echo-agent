import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Composer } from "../Composer";
import {
  createWebSpeechAsrProvider, registerAsrProvider, resetVoiceRegistry,
  type AsrProvider, type SpeechRecognitionLike,
} from "@/lib/voice-contract";

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: async () => () => {} }),
}));

const props = { streaming: false, apiReady: true, onSend: vi.fn(() => true), onCancel: vi.fn() };
const microphone = () => screen.getByRole("button", { name: "语音输入" });
const input = () => screen.getByRole("textbox");
const result = (text: string, isFinal = true) => ({ resultIndex: 0, results: [{ isFinal, 0: { transcript: text } }] });

function dictation() {
  const rounds: SpeechRecognitionLike[] = [];
  registerAsrProvider(createWebSpeechAsrProvider({
    isAvailable: () => true,
    createRecognition: () => {
      const rec: SpeechRecognitionLike = {
        lang: "", continuous: false, interimResults: false,
        start: vi.fn(), stop: vi.fn(), abort: vi.fn(),
        onresult: null, onerror: null, onend: null,
      };
      rounds.push(rec);
      return rec;
    },
  }));
  return rounds;
}

beforeEach(() => {
  localStorage.clear();
  resetVoiceRegistry();
  vi.clearAllMocks();
  vi.useFakeTimers();
});
afterEach(() => {
  cleanup();
  resetVoiceRegistry();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("Composer continuous voice input", () => {
  it("keeps listening after a pause and appends the second sentence without duplication or sending", () => {
    const rounds = dictation();
    const onToast = vi.fn();
    render(<Composer {...props} onToast={onToast} />);
    fireEvent.click(microphone());
    act(() => rounds[0].onresult?.(result("你好。")));
    act(() => {
      rounds[0].onend?.();
      vi.advanceTimersByTime(300);
    });
    expect(microphone()).toHaveAttribute("aria-pressed", "true");
    expect(microphone()).toHaveAttribute("title", "正在聆听…点击停止");
    act(() => {
      rounds[1].onresult?.(result("今天天气怎么样", false));
      rounds[1].onresult?.(result("今天天气怎么样？"));
      rounds[1].onresult?.(result("今天天气怎么样？"));
    });
    expect(input()).toHaveValue("你好。今天天气怎么样？");
    expect(props.onSend).not.toHaveBeenCalled();
    expect(onToast).not.toHaveBeenCalled();
  });

  it("ends on an explicit click, collects the final result, and does not restart", () => {
    const rounds = dictation();
    render(<Composer {...props} />);
    fireEvent.click(microphone());
    act(() => rounds[0].onresult?.(result("最后一句", false)));
    fireEvent.click(microphone());
    expect(rounds[0].stop).toHaveBeenCalledOnce();
    act(() => {
      rounds[0].onresult?.(result("最后一句。"));
      rounds[0].onend?.();
      vi.advanceTimersByTime(1000);
    });
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    expect(input()).toHaveValue("最后一句。");
    expect(rounds).toHaveLength(1);
  });

  it("can stop while a restart is pending and ignores callbacks from an earlier dictation", () => {
    const rounds = dictation();
    render(<Composer {...props} />);
    fireEvent.click(microphone());
    const lateResult = rounds[0].onresult!;
    act(() => {
      rounds[0].onresult?.(result("第一轮。"));
      rounds[0].onend?.();
    });
    fireEvent.click(microphone());
    act(() => vi.advanceTimersByTime(1000));
    expect(rounds).toHaveLength(1);
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(microphone());
    act(() => {
      rounds[1].onresult?.(result("第二轮。"));
      lateResult(result("旧录音的迟到文字"));
    });
    expect(input()).toHaveValue("第一轮。 第二轮。");
    expect(microphone()).toHaveAttribute("aria-pressed", "true");
  });

  it.each([false, true])("sending or enqueueing stops dictation and cannot repopulate the consumed draft (streaming=%s)", async streaming => {
    const rounds = dictation();
    const onEnqueue = vi.fn(() => true);
    render(<Composer {...props} streaming={streaming} onEnqueue={onEnqueue} />);
    fireEvent.click(microphone());
    const lateResult = rounds[0].onresult!;
    const lateEnd = rounds[0].onend!;
    act(() => rounds[0].onresult?.(result("待发送语音", false)));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: streaming ? "加入待发送队列" : "发送" })));
    expect(streaming ? onEnqueue : props.onSend).toHaveBeenCalledExactlyOnceWith("待发送语音", []);
    expect(rounds[0].abort).toHaveBeenCalledOnce();
    act(() => { lateResult(result("发送后的迟到文字")); lateEnd(); vi.advanceTimersByTime(1000); });
    expect(input()).toHaveValue("");
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    expect(rounds).toHaveLength(1);
  });

  it("switching drafts cancels a pending restart and unmount releases an active microphone", () => {
    const rounds = dictation();
    const view = render(<Composer {...props} draftKey="first" draft="" />);
    fireEvent.click(microphone());
    act(() => rounds[0].onend?.());
    view.rerender(<Composer {...props} draftKey="second" draft="其他任务的草稿" />);
    act(() => vi.advanceTimersByTime(1000));
    expect(rounds).toHaveLength(1);
    expect(input()).toHaveValue("其他任务的草稿");
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(microphone());
    view.unmount();
    expect(rounds[1].abort).toHaveBeenCalledOnce();
  });

  it("a permission failure stops the indicator and does not restart", () => {
    const rounds = dictation();
    const onToast = vi.fn();
    render(<Composer {...props} onToast={onToast} />);
    fireEvent.click(microphone());
    act(() => { rounds[0].onerror?.({ error: "not-allowed" }); vi.advanceTimersByTime(1000); });
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    expect(onToast).toHaveBeenCalledExactlyOnceWith("未授予麦克风或语音识别权限");
    expect(rounds).toHaveLength(1);
  });

  it("a synchronous startup error cannot turn the listening indicator back on", () => {
    const onToast = vi.fn();
    registerAsrProvider({ id: "startup-error", isAvailable: () => true, listen: (_, handlers) => {
      handlers.onError?.("start-failed");
      handlers.onEnd?.();
      return vi.fn<() => void>();
    } });
    render(<Composer {...props} onToast={onToast} />);
    fireEvent.click(microphone());
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    expect(onToast).toHaveBeenCalledExactlyOnceWith("无法启动语音识别，请重试");
  });

  it("an explicitly registered provider takes priority over the browser fallback", () => {
    const browserRecognition = vi.fn();
    vi.stubGlobal("SpeechRecognition", browserRecognition);
    const listen = vi.fn<AsrProvider["listen"]>(() => vi.fn<() => void>());
    registerAsrProvider({ id: "external", isAvailable: () => true, listen });
    render(<Composer {...props} />);
    fireEvent.click(microphone());
    expect(listen).toHaveBeenCalledOnce();
    expect(browserRecognition).not.toHaveBeenCalled();
    expect(microphone()).toHaveAttribute("aria-pressed", "true");
  });

  it("uses continuous browser recognition when no external provider is registered", () => {
    const rec: SpeechRecognitionLike = {
      lang: "", continuous: false, interimResults: false,
      start: vi.fn(), stop: vi.fn(), abort: vi.fn(),
      onresult: null, onerror: null, onend: null,
    };
    vi.stubGlobal("SpeechRecognition", class { constructor() { return rec; } });
    render(<Composer {...props} />);
    fireEvent.click(microphone());
    expect(rec.start).toHaveBeenCalledOnce();
    expect(rec.continuous).toBe(true);
    act(() => rec.onresult?.(result("浏览器语音")));
    expect(input()).toHaveValue("浏览器语音");
    expect(microphone()).toHaveAttribute("aria-pressed", "true");
  });

  it("cancels an external provider after an asynchronous error and rejects its late results", () => {
    let handlers!: Parameters<AsrProvider["listen"]>[1];
    const stopRecognition = Object.assign(vi.fn<() => void>(), { abort: vi.fn<() => void>() });
    registerAsrProvider({ id: "external-error", isAvailable: () => true, listen: (_, callbacks) => {
      handlers = callbacks;
      return stopRecognition;
    } });
    render(<Composer {...props} />);
    fireEvent.click(microphone());
    act(() => {
      handlers.onInterim?.("已有文字");
      handlers.onError?.("network");
      handlers.onFinal?.("错误后的迟到文字");
    });
    expect(stopRecognition.abort).toHaveBeenCalledOnce();
    expect(microphone()).toHaveAttribute("aria-pressed", "false");
    expect(input()).toHaveValue("已有文字");
  });
});
