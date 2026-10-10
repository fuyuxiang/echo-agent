import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useModalFocus } from "../use-modal-focus";

function FocusHarness({ onClose }: { onClose: () => void }) {
  const [menu, setMenu] = useState(false);
  const ref = useModalFocus<HTMLDivElement>(true, onClose);
  useEffect(() => {
    if (!menu) return;
    const dismiss = (event: Event) => {
      if (event.type !== "keydown" || (event as KeyboardEvent).key === "Escape") setMenu(false);
    };
    document.addEventListener("keydown", dismiss);
    document.addEventListener("mousedown", dismiss);
    document.getElementById("focus-option")?.focus();
    return () => {
      document.removeEventListener("keydown", dismiss);
      document.removeEventListener("mousedown", dismiss);
    };
  }, [menu]);
  return <>
    <div ref={ref} role="dialog" tabIndex={-1}>
      <button style={{ display: "none" }} data-modal-initial-focus>隐藏按钮</button>
      <div hidden><button>隐藏区块</button></div>
      <fieldset disabled><input aria-label="禁用域" /></fieldset>
      <input type="hidden" />
      <input aria-label="名称" />
      <button aria-controls="focus-menu" aria-expanded={menu} onClick={() => setMenu(true)}>模型</button>
      <button>保存</button>
    </div>
    {menu && createPortal(<div id="focus-menu" role="listbox"><button id="focus-option" role="option">模型 A</button></div>, document.body)}
  </>;
}

describe("useModalFocus", () => {
  it("初始和循环焦点跳过隐藏控件及 disabled fieldset", () => {
    render(<FocusHarness onClose={vi.fn()} />);
    const input = screen.getByRole("textbox", { name: "名称" });
    expect(input).toHaveFocus();
    fireEvent.keyDown(input, { key: "Tab", shiftKey: true });
    expect(screen.getByRole("button", { name: "保存" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(input).toHaveFocus();
  });

  it("输入法候选 Escape 和 229 不关闭模态", () => {
    const close = vi.fn();
    render(<FocusHarness onClose={close} />);
    const input = screen.getByRole("textbox", { name: "名称" });
    fireEvent.keyDown(input, { key: "Escape", isComposing: true });
    fireEvent.keyDown(input, { key: "Escape", keyCode: 229 });
    expect(close).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("portal 选择器先消费 Escape，Tab 回到模态相邻字段", () => {
    const close = vi.fn();
    render(<FocusHarness onClose={close} />);
    fireEvent.click(screen.getByRole("button", { name: "模型" }));
    fireEvent.keyDown(screen.getByRole("option"), { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(close).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "模型" }));
    fireEvent.keyDown(screen.getByRole("option"), { key: "Tab" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(screen.getByRole("button", { name: "保存" })).toHaveFocus();
  });
});
