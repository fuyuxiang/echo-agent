import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useTheiaWorkbenchBridge } from "../hooks/useTheiaWorkbenchBridge";

let width = 1200;
function Harness({ cwd = "/repo", active = true }: { cwd?: string; active?: boolean }) {
  const bridge = useTheiaWorkbenchBridge(cwd, [], undefined, active);
  return <div ref={element => {
    if (element) element.getBoundingClientRect = () => ({ width, right: width } as DOMRect);
    (bridge.workbenchRef as { current: HTMLDivElement | null }).current = element;
  }}>
    <div role="separator" onPointerDown={event => bridge.startAgentResize(event)}
      onKeyDown={event => { if (["ArrowLeft", "ArrowRight"].includes(event.key)) bridge.resizeAgentByKey(event.key); }} onDoubleClick={bridge.resetAgentWidth} />
    <output data-resizing={bridge.agentResizing}>{bridge.agentWidth}</output>
  </div>;
}
function pointer(type: string, x: number, id = 1) {
  const event = new Event(type, { bubbles: true });
  Object.assign(event, { clientX: x, pointerId: id, button: 0, buttons: type === "pointerup" ? 0 : 1, isPrimary: true });
  return event;
}
beforeEach(() => {
  width = 1200;
  localStorage.clear();
  // Exercise the hook's window-resize fallback without a synthetic observer.
  vi.stubGlobal("ResizeObserver", undefined);
});

it("临时缩小窗口不会覆盖面板偏好，恢复窗口后还原宽度", () => {
  localStorage.setItem("echo-agent-panel-width", "700");
  render(<Harness />);
  expect(screen.getByRole("status")).toHaveTextContent("700");
  width = 720;
  fireEvent(window, new Event("resize"));
  expect(Number(screen.getByRole("status").textContent)).toBeLessThan(410);
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("700");
  width = 1200;
  fireEvent(window, new Event("resize"));
  expect(screen.getByRole("status")).toHaveTextContent("700");
});

it.each(["blur", "pointercancel", "Escape", "workspace"])("%s 取消拖动，随后移动不再改变宽度", reason => {
  const view = render(<Harness />);
  fireEvent(screen.getByRole("separator"), pointer("pointerdown", 790));
  fireEvent(window, pointer("pointermove", 600));
  expect(screen.getByRole("status")).toHaveTextContent("600");
  if (reason === "Escape") fireEvent.keyDown(window, { key: "Escape" });
  else if (reason === "workspace") view.rerender(<Harness cwd="/other" />);
  else fireEvent(window, reason === "blur" ? new Event("blur") : pointer(reason, 600));
  fireEvent(window, pointer("pointermove", 500));
  expect(screen.getByRole("status")).toHaveTextContent("410");
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("410");
});

it("忽略其他指针，松开时提交最终坐标并释放捕获", () => {
  render(<Harness />);
  const handle = screen.getByRole("separator");
  handle.setPointerCapture = vi.fn();
  handle.releasePointerCapture = vi.fn();
  handle.hasPointerCapture = () => true;
  fireEvent(handle, pointer("pointerdown", 790));
  expect(handle.setPointerCapture).toHaveBeenCalledWith(1);
  fireEvent(window, pointer("pointermove", 500, 2));
  expect(screen.getByRole("status")).toHaveTextContent("410");
  act(() => { window.dispatchEvent(pointer("pointerup", 620)); });
  expect(screen.getByRole("status")).toHaveTextContent("580");
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("580");
  expect(handle.releasePointerCapture).toHaveBeenCalledWith(1);
});

it("保留的 Code 页面失活时取消拖动，重新进入后仍可调整宽度", () => {
  localStorage.setItem("echo-agent-panel-width", "700");
  const view = render(<Harness />);
  const handle = screen.getByRole("separator");
  handle.setPointerCapture = vi.fn();
  handle.releasePointerCapture = vi.fn();
  handle.hasPointerCapture = () => true;
  fireEvent(handle, pointer("pointerdown", 500));
  fireEvent(window, pointer("pointermove", 450));
  expect(screen.getByRole("status")).toHaveTextContent("750");
  view.rerender(<Harness active={false} />);
  expect(screen.getByRole("status")).toHaveAttribute("data-resizing", "false");
  expect(handle.releasePointerCapture).toHaveBeenCalledWith(1);
  fireEvent(window, pointer("pointermove", 600));
  fireEvent(window, pointer("pointerup", 600));
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("700");
  fireEvent(handle, pointer("pointerdown", 500));
  expect(screen.getByRole("status")).toHaveAttribute("data-resizing", "false");
  view.rerender(<Harness active />);
  expect(screen.getByRole("status")).toHaveTextContent("700");
  fireEvent(handle, pointer("pointerdown", 500));
  fireEvent(window, pointer("pointerup", 600));
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("600");
});

it.each(["zero-width", "hidden-ancestor"])("%s 时松开指针不会保存不可见面板的几何", reason => {
  localStorage.setItem("echo-agent-panel-width", "700");
  const view = render(<Harness />);
  fireEvent(screen.getByRole("separator"), pointer("pointerdown", 500));
  fireEvent(window, pointer("pointermove", 450));
  if (reason === "zero-width") width = 0;
  else view.container.hidden = true;
  fireEvent(window, pointer("pointerup", 600));
  view.container.hidden = false;
  expect(screen.getByRole("status")).toHaveTextContent("700");
  expect(screen.getByRole("status")).toHaveAttribute("data-resizing", "false");
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("700");
});

it("拖动期间的方向键和双击不会改变取消后恢复的偏好", () => {
  render(<Harness />);
  const handle = screen.getByRole("separator");
  fireEvent(handle, pointer("pointerdown", 790));
  fireEvent(window, pointer("pointermove", 600));
  fireEvent.keyDown(handle, { key: "ArrowLeft" });
  fireEvent.doubleClick(handle);
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.getByRole("status")).toHaveTextContent("410");
  expect(localStorage.getItem("echo-agent-panel-width")).toBe("410");
});
