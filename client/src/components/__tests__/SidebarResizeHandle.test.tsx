import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useContext, useLayoutEffect, useRef, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SidebarResizeHandle } from "../SidebarResizeHandle";
import { SidebarLayoutContext, SIDEBAR_WIDTH_KEY, useAppSidebarLayout } from "@/lib/sidebar-layout";

function Panel({ open, close }: { open: boolean; close: () => void }) {
  const context = useContext(SidebarLayoutContext)!;
  const closeRef = useRef(close);
  closeRef.current = close;
  useLayoutEffect(() => {
    context.registerPanel(open ? { minimumWidth: 280, close: () => closeRef.current() } : null);
    return () => context.registerPanel(null);
  }, [open, context.registerPanel]);
  return open ? <div data-testid="panel">工作区面板</div> : null;
}
// The reporter only needs the shared hook value; keep the fixture independent
// of file access and workspace tabs while exercising the real layout/handle.
function Harness() {
  const layout = useAppSidebarLayout();
  const [panelOpen, setPanelOpen] = useState(false);
  return <SidebarLayoutContext.Provider value={layout.context}>
    <div ref={layout.bodyRef} style={layout.style} className={layout.resizing ? "resizing" : ""} data-testid="body">
      <aside id="app-sidebar" className="sidebar" hidden={layout.collapsed}>
        <button aria-label="收起侧边栏" onClick={() => layout.setCollapsed(true)}>收起</button>
      </aside>
      <SidebarResizeHandle layout={layout} />
      {layout.collapsed && <button aria-label="展开侧边栏" onClick={() => layout.setCollapsed(false)}>展开</button>}
      <button onClick={() => setPanelOpen(true)}>打开面板</button>
      <button onClick={() => setPanelOpen(false)}>关闭面板</button>
      <Panel open={panelOpen} close={() => setPanelOpen(false)} />
      <output data-testid="panel-max">{layout.context.panelMaxWidth}</output>
    </div>
  </SidebarLayoutContext.Provider>;
}
function pointer(target: Element | Window, type: string, x: number, id = 1, button = 0) {
  const event = new MouseEvent(type, { bubbles: true, clientX: x, button });
  Object.defineProperty(event, "pointerId", { value: id });
  fireEvent(target, event);
}
const handle = () => screen.getByRole("separator", { name: "调整侧边栏宽度" });
const width = () => screen.getByTestId("body").style.getPropertyValue("--app-sidebar-width");
async function drag(from: number, to: number, finish = true) {
  pointer(handle(), "pointerdown", from);
  pointer(window, "pointermove", to);
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
  if (finish) pointer(window, "pointerup", to);
}
beforeEach(() => {
  localStorage.clear();
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1440 });
});
afterEach(() => vi.restoreAllMocks());

describe("全局侧栏宽度交互", () => {
  it("previews bounded widths and persists only the completed drag", async () => {
    const stored = vi.spyOn(Storage.prototype, "setItem");
    render(<Harness />);
    await drag(264, 336, false);
    expect(width()).toBe("336px");
    expect(localStorage.getItem(SIDEBAR_WIDTH_KEY)).toBe("264");
    expect(stored).toHaveBeenCalledTimes(1);
    pointer(window, "pointerup", 336);
    expect(localStorage.getItem(SIDEBAR_WIDTH_KEY)).toBe("336");
    await drag(336, 1100);
    expect(width()).toBe("400px");
    expect(handle()).toHaveAttribute("aria-valuenow", "400");
  });

  it("collapses only beyond the threshold on release, then restores the last committed width", async () => {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, "336");
    render(<Harness />);
    await drag(336, 200);
    expect(width()).toBe("240px");
    expect(screen.queryByLabelText("展开侧边栏")).not.toBeInTheDocument();
    await drag(240, 140, false);
    expect(screen.getByText("松开收起侧边栏")).toBeInTheDocument();
    expect(screen.queryByLabelText("展开侧边栏")).not.toBeInTheDocument();
    pointer(window, "pointerup", 140);
    expect(handle()).toHaveAttribute("aria-valuenow", "0");
    expect(handle()).toHaveFocus();
    const expand = screen.getByRole("button", { name: "展开侧边栏" });
    expand.focus();
    fireEvent.click(expand);
    expect(width()).toBe("240px");
    expect(screen.getByRole("button", { name: "收起侧边栏" })).toHaveFocus();
  });

  it.each(["Escape", "pointercancel", "blur", "lostpointercapture"])("rolls back a drag cancelled by %s", async (reason) => {
    render(<Harness />);
    await drag(264, 350, false);
    expect(width()).toBe("350px");
    if (reason === "Escape") fireEvent.keyDown(window, { key: reason });
    else if (reason === "blur") fireEvent.blur(window);
    else pointer(reason === "lostpointercapture" ? handle() : window, reason, 350);
    expect(width()).toBe("264px");
    expect(screen.getByTestId("body")).not.toHaveClass("resizing");
    expect(localStorage.getItem(SIDEBAR_WIDTH_KEY)).toBe("264");
    pointer(window, "pointermove", 390);
    expect(width()).toBe("264px");
  });

  it("ignores right-click and unrelated pointers", async () => {
    render(<Harness />);
    pointer(handle(), "pointerdown", 264, 1, 2);
    pointer(window, "pointermove", 350);
    expect(screen.getByTestId("body")).not.toHaveClass("resizing");
    pointer(handle(), "pointerdown", 264);
    pointer(window, "pointermove", 350, 2);
    pointer(window, "pointerup", 350, 2);
    expect(width()).toBe("264px");
    pointer(window, "pointercancel", 264);
  });

  it("supports keyboard resizing, collapse/restore and double-click reset", () => {
    render(<Harness />);
    fireEvent.keyDown(handle(), { key: "ArrowRight" });
    expect(width()).toBe("280px");
    fireEvent.keyDown(handle(), { key: "ArrowRight", shiftKey: true });
    expect(width()).toBe("320px");
    fireEvent.keyDown(handle(), { key: "End" });
    expect(width()).toBe("400px");
    fireEvent.keyDown(handle(), { key: "Enter" });
    expect(handle()).toHaveAttribute("aria-valuenow", "0");
    fireEvent.keyDown(handle(), { key: "ArrowRight" });
    expect(width()).toBe("400px");
    fireEvent.doubleClick(handle());
    expect(width()).toBe("264px");
    fireEvent.keyDown(handle(), { key: "ArrowLeft" });
    fireEvent.keyDown(handle(), { key: "ArrowLeft" });
    expect(width()).toBe("240px");
    fireEvent.keyDown(handle(), { key: "ArrowLeft" });
    expect(handle()).toHaveAttribute("aria-valuenow", "0");
    fireEvent.keyDown(handle(), { key: "Home" });
    expect(width()).toBe("264px");
  });

  it("restores from the collapsed edge and keeps cancellation harmless", async () => {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, "320");
    render(<Harness />);
    fireEvent.click(screen.getByRole("button", { name: "收起侧边栏" }));
    await drag(0, 60, false);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(handle()).toHaveAttribute("aria-valuenow", "0");
    await drag(0, 60);
    expect(width()).toBe("320px");
    fireEvent.click(screen.getByRole("button", { name: "收起侧边栏" }));
    await drag(0, 310);
    expect(width()).toBe("310px");
  });

  it("preserves preferred width through panel/window constraints and always allows explicit restore", async () => {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, "400");
    render(<Harness />);
    fireEvent.click(screen.getByText("打开面板"));
    expect(screen.getByTestId("panel-max")).toHaveTextContent("560");
    Object.defineProperty(window, "innerWidth", { value: 1024 });
    fireEvent(window, new Event("resize"));
    expect(width()).toBe("264px");
    expect(screen.getByTestId("panel-max")).toHaveTextContent("280");
    expect(localStorage.getItem(SIDEBAR_WIDTH_KEY)).toBe("400");
    Object.defineProperty(window, "innerWidth", { value: 900 });
    fireEvent(window, new Event("resize"));
    expect(handle()).toHaveAttribute("aria-valuenow", "0");
    fireEvent.click(screen.getByRole("button", { name: "展开侧边栏" }));
    await waitFor(() => expect(screen.queryByTestId("panel")).not.toBeInTheDocument());
    expect(width()).toBe("400px");
    expect(handle()).toHaveAttribute("aria-valuenow", "400");
  });

  it.each(["NaN", "Infinity", "-10", ""])("repairs invalid stored width %s", (value) => {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, value);
    render(<Harness />);
    expect(width()).toBe("264px");
    expect(localStorage.getItem(SIDEBAR_WIDTH_KEY)).toBe("264");
  });

  it("works when local storage is unavailable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("blocked"); });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    render(<Harness />);
    await drag(264, 340);
    expect(width()).toBe("340px");
  });
});
