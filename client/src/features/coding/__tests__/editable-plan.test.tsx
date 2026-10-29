import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EditablePlan } from "../agent/EditablePlan";

describe("EditablePlan generated results and user drafts", () => {
  beforeEach(() => localStorage.clear());

  it("adopts a newer generated plan until the user edits it", () => {
    const props = { scope: "task", busy: true, onExecute: vi.fn(), onSave: vi.fn() };
    const view = render(<EditablePlan {...props} initial="Previous answer" />);
    view.rerender(<EditablePlan {...props} initial="Actual generated plan" />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("Actual generated plan");
    fireEvent.change(screen.getByLabelText("编辑开发计划"), { target: { value: "User's final plan" } });
    view.rerender(<EditablePlan {...props} initial="Late model result" />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("User's final plan");
  });

  it("protects typing before the first result and executes or saves that exact text", () => {
    const onExecute = vi.fn().mockResolvedValue(true), onSave = vi.fn();
    const props = { scope: "task", onExecute, onSave };
    const view = render(<EditablePlan {...props} busy initial="" />);
    fireEvent.change(screen.getByLabelText("编辑开发计划"), { target: { value: "Final selected scope" } });
    view.rerender(<EditablePlan {...props} busy={false} initial="Generated late" />);
    fireEvent.click(screen.getByText("执行最终计划"));
    fireEvent.click(screen.getByText("保存到工程…"));
    expect(onExecute).toHaveBeenCalledWith("Final selected scope");
    expect(onSave).toHaveBeenCalledWith("Final selected scope");
  });

  it("persists the edited state across remounts without discarding legacy text", () => {
    localStorage.setItem("echo-coding-plan-v1:task", "Legacy user's plan");
    const props = { scope: "task", busy: false, onExecute: vi.fn(), onSave: vi.fn() };
    const view = render(<EditablePlan {...props} initial="Generated replacement" />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("Legacy user's plan");
    view.unmount();
    render(<EditablePlan {...props} initial="Another result" />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("Legacy user's plan");
    fireEvent.click(screen.getByText("采用最新方案"));
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("Another result");
  });
});
