import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SettingsSearch } from "../SettingsSearch";

describe("SettingsSearch", () => {
  it.each([
    { name: "组合输入标志", event: { isComposing: true } },
    { name: "输入法最终按键", event: { isComposing: false, keyCode: 229 } },
  ])("$name 不选择结果、移动选择或关闭搜索", ({ event }) => {
    const onSelect = vi.fn();
    render(<SettingsSearch onSelect={onSelect} />);
    const input = screen.getByRole("combobox", { name: "搜索设置" });
    fireEvent.change(input, { target: { value: "模型" } });
    const selected = input.getAttribute("aria-activedescendant");

    for (const key of ["ArrowDown", "ArrowUp", "Enter", "Escape"]) {
      expect(fireEvent.keyDown(input, { key, ...event })).toBe(true);
      expect(input).toHaveAttribute("aria-activedescendant", selected);
      expect(input).toHaveAttribute("aria-expanded", "true");
      expect(input).toHaveValue("模型");
    }
    expect(onSelect).not.toHaveBeenCalled();

    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.getAttribute("aria-activedescendant")).not.toBe(selected);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ section: "usage" }), expect.any(Function));
  });

  it("WebView 未提供组合标志时仍保留输入，结束组合后恢复正常搜索", () => {
    const onSelect = vi.fn((_entry, onSelected) => onSelected());
    render(<SettingsSearch onSelect={onSelect} />);
    const input = screen.getByRole("combobox", { name: "搜索设置" });
    fireEvent.change(input, { target: { value: "模型" } });
    const selected = input.getAttribute("aria-activedescendant");
    fireEvent.compositionStart(input);
    for (const key of ["ArrowDown", "ArrowUp", "Enter", "Escape"]) {
      expect(fireEvent.keyDown(input, { key, isComposing: false })).toBe(true);
      expect(input).toHaveAttribute("aria-activedescendant", selected);
      expect(input).toHaveAttribute("aria-expanded", "true");
    }
    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(onSelect).not.toHaveBeenCalled();
    expect(input).toHaveValue("模型");

    fireEvent.keyDown(input, { key: "Enter" });
    expect(onSelect).toHaveBeenCalledOnce();
    expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ section: "model" }), expect.any(Function));
    expect(input).toHaveValue("");
    expect(input).toHaveAttribute("aria-expanded", "false");
  });
});
