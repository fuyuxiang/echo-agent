import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WaitingIndicator } from "../WaitingIndicator";

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

describe("WaitingIndicator motion lifecycle", () => {
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

  it("shares one observer, pauses offscreen and releases resources after the final indicator unmounts", () => {
    const addListener = vi.spyOn(document, "addEventListener");
    const removeListener = vi.spyOn(document, "removeEventListener");
    const first = render(<WaitingIndicator />);
    const second = render(<WaitingIndicator />);
    const indicator = first.container.querySelector(".waiting-indicator")!;
    const other = second.container.querySelector(".waiting-indicator")!;
    expect(observers).toHaveLength(1);
    const observer = observers[0];
    expect(observer.observe).toHaveBeenCalledTimes(2);
    expect(addListener.mock.calls.filter(([event]) => event === "visibilitychange")).toHaveLength(1);
    expect(indicator).toHaveAttribute("data-motion", "off");
    act(() => observer.intersect(indicator, 1));
    expect(indicator).toHaveAttribute("data-motion", "on");
    expect(other).toHaveAttribute("data-motion", "off");
    act(() => observer.intersect(indicator, 0.05));
    expect(indicator).toHaveAttribute("data-motion", "off");
    first.unmount();
    expect(observer.unobserve).toHaveBeenCalledWith(indicator);
    expect(observer.disconnect).not.toHaveBeenCalled();
    second.unmount();
    expect(observer.disconnect).toHaveBeenCalledOnce();
    expect(removeListener).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
    act(() => observer.intersect(indicator, 1));
    expect(indicator).toHaveAttribute("data-motion", "off");
  });

  it("only resumes visible indicators when returning from the background", () => {
    const view = render(<><WaitingIndicator /><WaitingIndicator /></>);
    const [visible, offscreen] = view.container.querySelectorAll(".waiting-indicator");
    act(() => observers[0].intersect(visible, 1));
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(visible).toHaveAttribute("data-motion", "off");
    vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(visible).toHaveAttribute("data-motion", "on");
    expect(offscreen).toHaveAttribute("data-motion", "off");
  });

  it("still follows document visibility when IntersectionObserver is unavailable", () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const view = render(<WaitingIndicator />);
    const indicator = view.container.querySelector(".waiting-indicator")!;
    expect(indicator).toHaveAttribute("data-motion", "on");
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    expect(indicator).toHaveAttribute("data-motion", "off");
  });
});
