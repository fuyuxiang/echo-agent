import { act, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useStickToBottom } from "../use-stick-to-bottom";

let resizeObservers: ResizeObserverMock[] = [];
let animationFrames = new Map<number, FrameRequestCallback>();
let nextAnimationFrame = 1;

class ResizeObserverMock {
  readonly observed = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {
    resizeObservers.push(this);
  }

  observe = (target: Element) => {
    this.observed.add(target);
  };

  unobserve = (target: Element) => {
    this.observed.delete(target);
  };

  disconnect = () => {
    this.observed.clear();
  };

  trigger() {
    this.callback([], this as unknown as ResizeObserver);
  }
}

function flushAnimationFrames() {
  const frames = [...animationFrames.entries()];
  animationFrames.clear();
  for (const [, callback] of frames) callback(performance.now());
}

function setScrollMetrics(
  element: HTMLElement,
  { scrollHeight, clientHeight, scrollTop }: {
    scrollHeight: number;
    clientHeight: number;
    scrollTop: number;
  },
) {
  Object.defineProperties(element, {
    scrollHeight: { configurable: true, value: scrollHeight },
    clientHeight: { configurable: true, value: clientHeight },
    scrollTop: { configurable: true, writable: true, value: scrollTop },
  });
}

function Harness({
  version,
  streaming = false,
  sessionId = "session-1",
}: {
  version: number;
  streaming?: boolean;
  sessionId?: string;
}) {
  const {
    scrollRef,
    contentRef,
    following,
    pauseFollowing,
    scrollToBottom,
  } = useStickToBottom({
    contentVersion: version,
    streaming,
    sessionId,
  });

  return (
    <>
      <div ref={scrollRef} data-testid="scroll">
        <div ref={contentRef} data-testid="content"><div data-msg-id="reading-anchor">message</div>
          <button type="button" data-chat-copy="true" data-testid="copy-action">
            copy
          </button>
          <button type="button" data-chat-reading="true" data-testid="record-action">执行记录</button>
        </div>
      </div>
      <button type="button" data-testid="pause" onClick={pauseFollowing}>pause</button>
      <button type="button" data-testid="resume" onClick={scrollToBottom}>resume</button>
      <output data-testid="following">{String(following)}</output>
    </>
  );
}

describe("useStickToBottom", () => {
  beforeEach(() => {
    resizeObservers = [];
    animationFrames = new Map();
    nextAnimationFrame = 1;
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      const id = nextAnimationFrame++;
      animationFrames.set(id, callback);
      return id;
    });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => {
      animationFrames.delete(id);
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("follows transcript state changes and asynchronous content growth", () => {
    const { getByTestId, rerender } = render(<Harness version={1} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_250,
    });
    act(() => resizeObservers[0].trigger());
    expect(scroller.scrollTop).toBe(850);

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_400,
    });
    rerender(<Harness version={2} />);
    expect(scroller.scrollTop).toBe(1_000);

    act(flushAnimationFrames);
    expect(scroller.scrollTop).toBe(1_000);
  });

  it("keeps following when reflow clamps scrolling to a smaller bottom before ResizeObserver", () => {
    const { getByTestId } = render(<Harness version={1} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, { scrollHeight: 1600, clientHeight: 400, scrollTop: 1200 });
    fireEvent.click(getByTestId("resume"));
    act(flushAnimationFrames);
    setScrollMetrics(scroller, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });
    fireEvent.scroll(scroller);
    expect(getByTestId("following")).toHaveTextContent("true");
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1300 });
    act(() => resizeObservers[0].trigger());
    expect(scroller.scrollTop).toBe(900);
    // A real one-pixel upward scrollbar move must still stop following.
    scroller.scrollTop = 899;
    fireEvent.scroll(scroller);
    expect(getByTestId("following")).toHaveTextContent("false");
  });

  it("tracks viewport changes caused by a resizing composer", () => {
    const { getByTestId } = render(<Harness version={1} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    Object.defineProperty(scroller, "clientHeight", {
      configurable: true,
      value: 300,
    });
    act(() => resizeObservers[0].trigger());

    expect(scroller.scrollTop).toBe(700);
  });

  it("does not steal the position after the user deliberately scrolls up", () => {
    const { getByTestId, rerender } = render(<Harness version={1} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });
    fireEvent.scroll(scroller);
    scroller.scrollTop = 200;
    fireEvent.scroll(scroller);

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_300,
    });
    act(() => resizeObservers[0].trigger());
    rerender(<Harness version={2} />);

    expect(scroller.scrollTop).toBe(200);
  });

  it("a small upward wheel gesture cancels follow before the next streamed update", () => {
    const { getByTestId, rerender } = render(<Harness version={1} streaming />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    // Real trackpads start with small deltas. The old 80px near-bottom rule
    // kept follow enabled here and the next token snapped straight back down.
    fireEvent.wheel(scroller, { deltaY: -12 });
    scroller.scrollTop = 588;
    fireEvent.scroll(scroller);

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_100,
    });
    act(() => resizeObservers[0].trigger());
    rerender(<Harness version={2} streaming />);
    act(flushAnimationFrames);

    expect(scroller.scrollTop).toBe(588);
  });

  it("scrolling upward cancels an already scheduled bottom alignment", () => {
    const { getByTestId } = render(<Harness version={1} streaming />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });
    act(() => resizeObservers[0].trigger());

    fireEvent.wheel(scroller, { deltaY: -8 });
    scroller.scrollTop = 592;
    fireEvent.scroll(scroller);
    act(flushAnimationFrames);

    expect(scroller.scrollTop).toBe(592);
  });

  it.each([true, false])("opening records preserves their first stage before layout grows (streaming=%s)", streaming => {
    const { getByTestId, rerender } = render(<Harness version={1} streaming={streaming} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, { scrollHeight: 1_000, clientHeight: 400, scrollTop: 600 });
    fireEvent.click(getByTestId("record-action"));
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 1_800 });
    act(() => resizeObservers[0].trigger());
    rerender(<Harness version={2} streaming={streaming} />);
    act(flushAnimationFrames);
    expect(scroller.scrollTop).toBe(600);
    expect(getByTestId("following")).toHaveTextContent("false");
    fireEvent.click(getByTestId("resume"));
    expect(scroller.scrollTop).toBe(1_400);
  });

  it("copying from the transcript pauses following before streamed content grows", () => {
    const { getByTestId, rerender } = render(<Harness version={1} streaming />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    fireEvent.pointerDown(getByTestId("copy-action"));
    expect(getByTestId("following")).toHaveTextContent("false");

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_200,
    });
    act(() => resizeObservers[0].trigger());
    rerender(<Harness version={2} streaming />);
    act(flushAnimationFrames);

    expect(scroller.scrollTop).toBe(600);
    expect(getByTestId("following")).toHaveTextContent("false");

    fireEvent.click(getByTestId("resume"));
    expect(scroller.scrollTop).toBe(800);
    expect(getByTestId("following")).toHaveTextContent("true");
  });

  it("keyboard or context-menu copy of selected transcript text pauses following", () => {
    const { getByTestId } = render(<Harness version={1} streaming />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    fireEvent.copy(getByTestId("content"));

    expect(getByTestId("following")).toHaveTextContent("false");
  });

  it("keyboard activation of a transcript copy button pauses following", () => {
    const { getByTestId } = render(<Harness version={1} streaming />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    fireEvent.click(getByTestId("copy-action"));

    expect(getByTestId("following")).toHaveTextContent("false");
  });

  it("copying a completed conversation does not change future follow intent", () => {
    const { getByTestId } = render(<Harness version={1} streaming={false} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });

    fireEvent.pointerDown(getByTestId("copy-action"));

    expect(getByTestId("following")).toHaveTextContent("true");
  });

  it("resumes following when the user returns near the bottom", () => {
    const { getByTestId } = render(<Harness version={1} />);
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });
    fireEvent.scroll(scroller);
    scroller.scrollTop = 200;
    fireEvent.scroll(scroller);

    scroller.scrollTop = 600;
    fireEvent.scroll(scroller);
    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_200,
    });
    act(() => resizeObservers[0].trigger());

    expect(scroller.scrollTop).toBe(800);
  });

  it("does not steal the reader's position when a queued response starts", () => {
    const { getByTestId, rerender } = render(
      <Harness version={1} streaming={false} />,
    );
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 1_000,
      clientHeight: 400,
      scrollTop: 600,
    });
    fireEvent.scroll(scroller);
    scroller.scrollTop = 100;
    fireEvent.scroll(scroller);

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_200,
    });
    rerender(<Harness version={2} streaming />);

    expect(scroller.scrollTop).toBe(100);
  });

  it("starts each switched conversation at its bottom", () => {
    const { getByTestId, rerender } = render(
      <Harness version={1} sessionId="session-1" />,
    );
    const scroller = getByTestId("scroll");
    setScrollMetrics(scroller, {
      scrollHeight: 900,
      clientHeight: 400,
      scrollTop: 50,
    });
    fireEvent.scroll(scroller);

    Object.defineProperty(scroller, "scrollHeight", {
      configurable: true,
      value: 1_100,
    });
    rerender(<Harness version={2} sessionId="session-2" />);

    expect(scroller.scrollTop).toBe(700);
  });
  it("preserves a visible message across width reflow and restores each session's reading mode", () => {
    const { getByTestId, rerender, container } = render(<Harness version={1} sessionId="A" />);
    const viewport = getByTestId("scroll");
    setScrollMetrics(viewport,{scrollHeight:2000,clientHeight:400,scrollTop:600});
    const row = container.querySelector<HTMLElement>("[data-msg-id]")!;
    let contentTop = 650;
    vi.spyOn(row,"getBoundingClientRect").mockImplementation(()=>({top:contentTop-viewport.scrollTop,bottom:contentTop+200-viewport.scrollTop} as DOMRect));
    fireEvent.wheel(viewport,{deltaY:-1}); fireEvent.scroll(viewport);
    expect(getByTestId("following")).toHaveTextContent("false");
    contentTop = 850;
    act(()=>resizeObservers.forEach(observer=>observer.trigger()));
    expect(viewport.scrollTop).toBe(800);
    rerender(<Harness version={2} sessionId="B" />);
    expect(getByTestId("following")).toHaveTextContent("true");
    expect(viewport.scrollTop).toBe(1600);
    rerender(<Harness version={3} sessionId="A" />);
    expect(viewport.scrollTop).toBe(800);
    expect(getByTestId("following")).toHaveTextContent("false");
  });

});
