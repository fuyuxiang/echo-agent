import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ResourceList, ResourceMenu, ResourceRow } from "../ResourceList";

function Collection({ scope = "experts", defaultView }: { scope?: "experts" | "skills" | "connectors"; defaultView?: "cards" | "list" }) {
  return <ResourceList collectionKey={scope} defaultView={defaultView} aria-label="测试资源">
    <ResourceRow><button type="button">打开资源</button></ResourceRow>
  </ResourceList>;
}

describe("资源视图偏好与操作", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.restoreAllMocks());

  it.each([
    [null, "list"], ["invalid", "list"], ["cards", "cards"], ["list", "list"],
  ])("连接器默认列表，保留合法偏好（%s → %s），读取时不覆盖存储", (saved, expected) => {
    if (saved !== null) localStorage.setItem("echo.resource-view.connectors", saved);
    const save = vi.spyOn(Storage.prototype, "setItem");
    render(<Collection scope="connectors" defaultView="list" />);
    expect(screen.getByRole("list")).toHaveAttribute("data-view", expected);
    expect(save).not.toHaveBeenCalled();
  });

  it("无法读取偏好时使用页面默认值，清除跨窗口偏好也回到页面默认值", () => {
    const read = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("denied"); });
    render(<Collection scope="connectors" defaultView="list" />);
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "list");
    read.mockRestore();
    fireEvent.click(screen.getByRole("button", { name: "卡片视图" }));
    localStorage.removeItem("echo.resource-view.connectors");
    fireEvent(window, new StorageEvent("storage", { key: null }));
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "list");
  });

  it("默认卡片；切换保留条目与焦点，重新打开仍使用所选视图，各页互不影响", async () => {
    const user = userEvent.setup();
    const { unmount } = render(<Collection />);
    const item = screen.getByRole("button", { name: "打开资源" });
    expect(screen.getByRole("button", { name: "卡片视图" })).toHaveAttribute("aria-pressed", "true");
    const listButton = screen.getByRole("button", { name: "列表视图" });
    listButton.focus();
    await user.keyboard("{Enter}");
    expect(listButton).toHaveFocus();
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "list");
    expect(screen.getByRole("button", { name: "打开资源" })).toBe(item);
    unmount();
    const next = render(<><Collection /><Collection scope="skills" /></>);
    const lists = screen.getAllByRole("list");
    expect(lists[0]).toHaveAttribute("data-view", "list");
    expect(lists[1]).toHaveAttribute("data-view", "cards");
    expect(within(lists[0]).getByRole("listitem")).toBeInTheDocument();
    next.unmount();
  });

  it("存储被禁用或偏好损坏时仍能正常切换", () => {
    localStorage.setItem("echo.resource-view.experts", "invalid");
    render(<Collection />);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("denied"); });
    fireEvent.click(screen.getByRole("button", { name: "列表视图" }));
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "list");
    fireEvent.click(screen.getByRole("button", { name: "卡片视图" }));
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "cards");
  });

  it("无法读取偏好时仍能打开页面并使用列表", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("denied"); });
    render(<Collection />);
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "cards");
    fireEvent.click(screen.getByRole("button", { name: "列表视图" }));
    expect(screen.getByRole("list")).toHaveAttribute("data-view", "list");
  });

  it("更多菜单支持键盘导航、禁用动作及 Escape 返回原卡片", async () => {
    const select = vi.fn();
    const user = userEvent.setup();
    render(<ResourceMenu label="专家 评审" actions={[
      { label: "编辑", onSelect: select },
      { label: "删除", disabled: true, onSelect: select },
    ]} />);
    const trigger = screen.getByRole("button", { name: "专家 评审更多操作" });
    trigger.focus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitem", { name: "编辑" })).toHaveFocus();
    expect(screen.getByRole("menuitem", { name: "删除" })).toBeDisabled();
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
    expect(screen.queryByRole("menu")).toBeNull();
    await user.keyboard("{Enter}{Enter}");
    expect(select).toHaveBeenCalledOnce();
    expect(trigger).toHaveFocus();
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
