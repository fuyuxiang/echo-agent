import { fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { Select } from "../Select";
import { useModalFocus } from "@/lib/use-modal-focus";

function Field() {
  const [value, setValue] = useState("all");
  return <label>项目范围<Select name="scope" value={value} onChange={event => setValue(event.target.value)}>
    <option value="all">全部项目</option><option value="private">仅自己</option><option value="shared">已共享</option>
  </Select></label>;
}

describe("Select", () => {
  it("opens a portalled list and emits the original form value when choosing", () => {
    const { container } = render(<form><Field /></form>);
    const field = screen.getByRole("combobox", { name: "项目范围" });
    fireEvent.click(field);
    const list = screen.getByRole("listbox", { name: "项目范围" });
    expect(list.parentElement).toBe(document.body);
    expect(field).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(within(list).getByRole("option", { name: "仅自己" }));
    expect(field).toHaveValue("private");
    expect(new FormData(container.querySelector("form")!).get("scope")).toBe("private");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(field).toHaveFocus();
  });

  it("supports arrows, Home/End and Enter without choosing disabled options", () => {
    const change = vi.fn();
    render(<Select aria-label="模型" defaultValue="a" onChange={change}>
      <option value="a">Alpha</option><optgroup label="组织" disabled><option value="b">Beta</option></optgroup>
      <option value="c">Charlie</option><option value="d">Delta</option>
    </Select>);
    const field = screen.getByRole("combobox");
    fireEvent.keyDown(field, { key: "ArrowDown" });
    expect(within(screen.getByRole("listbox")).getByRole("option", { name: "Beta" })).toHaveAttribute("aria-disabled", "true");
    fireEvent.keyDown(field, { key: "ArrowDown" });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(field).toHaveValue("c");
    expect(change).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(field, { key: "End" });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(field).toHaveValue("d");
    fireEvent.keyDown(field, { key: "Home" });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(field).toHaveValue("a");
  });

  it("cancels on Escape, Tab or an outside press without changing the selection", () => {
    render(<Field />);
    const field = screen.getByRole("combobox");
    for (const key of ["Escape", "Tab"]) {
      fireEvent.click(field);
      fireEvent.keyDown(field, { key: "ArrowDown" });
      fireEvent.keyDown(field, { key });
      expect(screen.queryByRole("listbox")).toBeNull();
      expect(field).toHaveValue("all");
    }
    fireEvent.click(field);
    fireEvent.mouseDown(document.body);
    expect(screen.queryByRole("listbox")).toBeNull();
    fireEvent.click(field);
    fireEvent.blur(window);
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("supports typeahead, required validation and disabled fields", () => {
    const { rerender } = render(<Select aria-label="模型" required defaultValue="">
      <option value="" disabled>选择模型</option><option value="a">Alpha</option><option value="b">Beta</option>
    </Select>);
    const field = screen.getByRole("combobox") as HTMLSelectElement;
    expect(field.checkValidity()).toBe(false);
    fireEvent.keyDown(field, { key: "b" });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(field).toHaveValue("b");
    expect(field.checkValidity()).toBe(true);
    rerender(<Select aria-label="模型" disabled><option>Alpha</option></Select>);
    fireEvent.click(field);
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("gives the open popup first refusal of Escape inside a modal", () => {
    const close = vi.fn();
    function Dialog() {
      const ref = useModalFocus<HTMLDivElement>(true, close);
      return <div ref={ref} role="dialog"><Field /><button>保存</button></div>;
    }
    render(<Dialog />);
    const field = screen.getByRole("combobox");
    fireEvent.click(field);
    fireEvent.keyDown(field, { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(close).not.toHaveBeenCalled();
    fireEvent.keyDown(field, { key: "Escape" });
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("reconciles changed options and keeps native multi-selects functional", () => {
    const { rerender } = render(<Select aria-label="模型"><option value="a">Alpha</option></Select>);
    fireEvent.click(screen.getByRole("combobox"));
    rerender(<Select aria-label="模型"><option value="b">Beta</option></Select>);
    const list = screen.getByRole("listbox");
    expect(within(list).queryByRole("option", { name: "Alpha" })).toBeNull();
    fireEvent.click(within(list).getByRole("option", { name: "Beta" }));
    expect(screen.getByRole("combobox")).toHaveValue("b");
    rerender(<Select aria-label="多选" multiple defaultValue={["a"]}><option value="a">Alpha</option><option value="b">Beta</option></Select>);
    expect(screen.getByRole("listbox", { name: "多选" })).not.toHaveAttribute("aria-expanded");
  });
});
