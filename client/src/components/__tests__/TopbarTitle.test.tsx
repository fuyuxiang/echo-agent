import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { TopbarTitle } from "../TopbarTitle";

describe("TopbarTitle", () => {
  it("确认输入法候选不会提交或取消标题编辑", async () => {
    const onRename = vi.fn().mockResolvedValue(undefined);
    render(<TopbarTitle title="原标题" onRename={onRename} />);
    fireEvent.click(screen.getByRole("button", { name: "编辑标题" }));
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "中文标题" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Escape", isComposing: true });
    expect(onRename).not.toHaveBeenCalled();
    expect(input).toHaveValue("中文标题");
    await act(async () => fireEvent.keyDown(input, { key: "Enter" }));
    expect(onRename).toHaveBeenCalledOnce();
    expect(onRename).toHaveBeenCalledWith("中文标题");
  });

  it("Enter 与失焦只提交一次，保存完成前禁用再次编辑", async () => {
    let finish!: () => void;
    const onRename = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<TopbarTitle title="原标题" onRename={onRename} />);
    fireEvent.click(screen.getByRole("button", { name: "编辑标题" }));
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "新标题" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.blur(input);
    expect(onRename).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "编辑标题" })).toBeDisabled();
    await act(async () => finish());
    expect(screen.getByRole("button", { name: "编辑标题" })).toBeEnabled();
  });

  it("Escape 后的失焦不会误提交被取消的草稿", () => {
    const onRename = vi.fn();
    render(<TopbarTitle title="原标题" onRename={onRename} />);
    fireEvent.click(screen.getByRole("button", { name: "编辑标题" }));
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "应取消" } });
    fireEvent.keyDown(input, { key: "Escape" });
    fireEvent.blur(input);
    expect(onRename).not.toHaveBeenCalled();
    expect(screen.getByText("原标题")).toBeInTheDocument();
  });
});
