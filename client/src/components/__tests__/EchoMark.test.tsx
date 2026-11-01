import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EchoMark } from "../EchoMark";

const observers: MockIntersectionObserver[] = [];
class MockIntersectionObserver {
  root = null;
  rootMargin = "0px";
  thresholds = [0.1];
  observe = vi.fn();
  unobserve = vi.fn();
  disconnect = vi.fn();
  takeRecords = () => [];
  constructor(private callback: IntersectionObserverCallback) { observers.push(this); }
  intersect(target: Element, intersectionRatio: number) {
    this.callback([{ target, intersectionRatio, isIntersecting: intersectionRatio > 0 } as IntersectionObserverEntry], this);
  }
}

describe("EchoMark motion lifecycle", () => {
  beforeEach(() => {
    observers.length = 0;
    vi.stubGlobal("IntersectionObserver", MockIntersectionObserver);
    vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("shares one observer, pauses offscreen and releases resources after the final mark unmounts", () => {
    const addListener = vi.spyOn(document, "addEventListener");
    const removeListener = vi.spyOn(document, "removeEventListener");
    const first = render(<EchoMark active />);
    const second = render(<EchoMark active />);
    const mark = first.container.querySelector(".echo-mark")!;
    const other = second.container.querySelector(".echo-mark")!;
    expect(observers).toHaveLength(1);
    const observer = observers[0];
    expect(observer.observe).toHaveBeenCalledTimes(2);
    expect(addListener.mock.calls.filter(([event]) => event === "visibilitychange")).toHaveLength(1);
    expect(mark).toHaveAttribute("data-motion", "off");
    act(() => observer.intersect(mark, 1));
    expect(mark).toHaveAttribute("data-motion", "on");
    expect(other).toHaveAttribute("data-motion", "off");
    act(() => observer.intersect(mark, 0.05));
    expect(mark).toHaveAttribute("data-motion", "off");
    first.unmount();
    expect(observer.unobserve).toHaveBeenCalledWith(mark);
    expect(observer.disconnect).not.toHaveBeenCalled();
    second.unmount();
    expect(observer.disconnect).toHaveBeenCalledOnce();
    expect(removeListener).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
    act(() => observer.intersect(mark, 1));
    expect(mark).toHaveAttribute("data-motion", "off");
  });

  it("only resumes visible marks when returning from the background", () => {
    const view = render(<><EchoMark active /><EchoMark active /></>);
    const [visible, offscreen] = view.container.querySelectorAll(".echo-mark");
    act(() => observers[0].intersect(visible, 1));
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(visible).toHaveAttribute("data-motion", "off");
    vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(visible).toHaveAttribute("data-motion", "on");
    expect(offscreen).toHaveAttribute("data-motion", "off");
  });

  it("keeps the shared app artwork intact and stops motion when its reply finishes", () => {
    const view = render(<><EchoMark active /><EchoMark active /></>);
    const [mark, other] = view.container.querySelectorAll(".echo-mark");
    const artwork = mark.querySelector("img")!;
    expect(artwork).toHaveAttribute("src", "/app-icon.png");
    expect(artwork).toHaveAttribute("alt", "");
    expect(artwork).toHaveAttribute("draggable", "false");
    expect(other.querySelector("img")).toHaveAttribute("src", "/app-icon.png");
    expect(mark).toHaveAttribute("aria-hidden", "true");
    expect(mark).toHaveClass("echo-mark--active");
    act(() => observers[0].intersect(mark, 1));
    view.rerender(<><EchoMark /><EchoMark active /></>);
    expect(mark).not.toHaveClass("echo-mark--active");
    expect(mark).toHaveAttribute("data-motion", "off");
    expect(mark.querySelector("img")).toBe(artwork);
    expect(observers[0].observe).toHaveBeenCalledTimes(2);
    expect(observers[0].unobserve).toHaveBeenCalledWith(mark);
    act(() => observers[0].intersect(mark, 1));
    expect(mark).toHaveAttribute("data-motion", "off");
  });

  it("completed history needs no observer and resumes observation only when active", () => {
    const view = render(<EchoMark />);
    const mark = view.container.querySelector(".echo-mark")!;
    expect(observers).toHaveLength(0);
    expect(mark).toHaveAttribute("data-motion", "off");
    view.rerender(<EchoMark active />);
    expect(observers).toHaveLength(1);
    act(() => observers[0].intersect(mark, 1));
    expect(mark).toHaveAttribute("data-motion", "on");
    view.rerender(<EchoMark />);
    expect(mark).toHaveAttribute("data-motion", "off");
    expect(observers[0].disconnect).toHaveBeenCalledOnce();
  });

  it("still follows document visibility when IntersectionObserver is unavailable", () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const view = render(<EchoMark active />);
    const mark = view.container.querySelector(".echo-mark")!;
    expect(mark).toHaveAttribute("data-motion", "on");
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(mark).toHaveAttribute("data-motion", "off");
  });
});
