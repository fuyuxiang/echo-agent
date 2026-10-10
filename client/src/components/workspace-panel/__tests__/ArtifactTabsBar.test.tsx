import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { ArtifactTabsBar } from "../ArtifactTabsBar";
import type { UnifiedTab } from "@/lib/use-unified-tabs";

const tabs: UnifiedTab[] = ["a.md", "b.ts", "c.txt"].map((label, index) => ({ id: `tab-${index}`, kind: "file", label, viewWhenActive: "fileTree" }));

describe("ArtifactTabsBar keyboard navigation", () => {
  beforeEach(() => { HTMLElement.prototype.scrollIntoView = vi.fn(); });
  function Fixture() {
    const [items, setItems] = useState(tabs);
    const [active, setActive] = useState(tabs[0].id);
    return <ArtifactTabsBar tabs={items} activeTabId={active} onSelect={setActive} onClose={(id) => setItems((current) => current.filter((tab) => tab.id !== id))} />;
  }

  it("箭头及 Home/End 切换标签并保留一个 tab stop", () => {
    render(<Fixture />);
    const first = screen.getByRole("tab", { name: /a.md/ });
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowRight" });
    const second = screen.getByRole("tab", { name: /b.ts/ });
    expect(second).toHaveFocus();
    expect(second).toHaveAttribute("aria-selected", "true");
    expect(first).toHaveAttribute("tabindex", "-1");
    fireEvent.keyDown(second, { key: "End" });
    const last = screen.getByRole("tab", { name: /c.txt/ });
    expect(last).toHaveFocus();
    fireEvent.keyDown(last, { key: "ArrowRight" });
    expect(first).toHaveFocus();
    fireEvent.keyDown(first, { key: "Home" });
    expect(first).toHaveFocus();
  });

  it("Delete 关闭后焦点转移到相邻标签", () => {
    render(<Fixture />);
    const first = screen.getByRole("tab", { name: /a.md/ });
    first.focus();
    fireEvent.keyDown(first, { key: "Delete" });
    expect(screen.queryByRole("tab", { name: /a.md/ })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /b.ts/ })).toHaveFocus();
    expect(screen.getByRole("tab", { name: /b.ts/ })).toHaveAttribute("aria-selected", "true");
  });

  it("关闭按钮键盘事件不选中父标签", () => {
    const onSelect = vi.fn();
    render(<ArtifactTabsBar tabs={tabs} activeTabId={tabs[0].id} onSelect={onSelect} onClose={vi.fn()} />);
    const close = screen.getByRole("button", { name: "关闭标签 b.ts" });
    fireEvent.keyDown(close, { key: "Enter" });
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("聚焦关闭按钮激活后恢复相邻标签焦点", () => {
    render(<Fixture />);
    const close = screen.getByRole("button", { name: "关闭标签 a.md" });
    close.focus();
    fireEvent.click(close, { detail: 0 });
    expect(screen.getByRole("tab", { name: /b.ts/ })).toHaveFocus();
    expect(screen.getByRole("tab", { name: /b.ts/ })).toHaveAttribute("aria-selected", "true");
  });
});
