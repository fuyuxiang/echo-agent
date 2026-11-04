import { fireEvent, render, screen } from "@testing-library/react";
import { useLayoutEffect } from "react";
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

  it("clears generated drafts on interruption and explains how to continue", () => {
    const props = { scope: "task", busy: false, onExecute: vi.fn(), onSave: vi.fn() };
    const view = render(<EditablePlan {...props} initial="Earlier generated plan" />);
    view.rerender(<EditablePlan {...props} initial="" interrupted />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("");
    expect(screen.getByRole("status")).toHaveTextContent("本轮计划未完整生成");
    expect(screen.getByRole("button", { name: "执行最终计划" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "保存到工程…" })).toBeDisabled();
    view.unmount();
    render(<EditablePlan {...props} initial="" interrupted />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("");
  });

  it("retains a user's draft on interruption and adopts a later complete plan only on request", () => {
    const onExecute = vi.fn().mockResolvedValue(true);
    const props = { scope: "task", busy: false, onExecute, onSave: vi.fn() };
    const view = render(<EditablePlan {...props} initial="" />);
    fireEvent.change(screen.getByLabelText("编辑开发计划"), { target: { value: "User's complete plan" } });
    view.rerender(<EditablePlan {...props} initial="" interrupted />);
    expect(screen.getByRole("status")).toHaveTextContent("你的草稿已保留，请核对后执行");
    fireEvent.click(screen.getByRole("button", { name: "执行最终计划" }));
    expect(onExecute).toHaveBeenCalledWith("User's complete plan");
    view.unmount();
    render(<EditablePlan {...props} initial="New complete plan" />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("User's complete plan");
    fireEvent.click(screen.getByRole("button", { name: "采用最新方案" }));
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("New complete plan");
  });

  it("does not restore an old automatic draft when the latest Plan was interrupted", () => {
    localStorage.setItem("echo-coding-plan-v1:task", JSON.stringify({
      kind: "echo-coding-plan-draft", text: "Partial response persisted by an older version", edited: false,
    }));
    render(<EditablePlan scope="task" initial="" interrupted busy={false} onExecute={vi.fn()} onSave={vi.fn()} />);
    expect(screen.getByLabelText("编辑开发计划")).toHaveValue("");
    expect(screen.getByRole("button", { name: "执行最终计划" })).toBeDisabled();
  });

  it("disables the old automatic plan in the same render that announces interruption", () => {
    const committed: { text: string; disabled: boolean }[] = [];
    function Panel({ interrupted }: { interrupted: boolean }) {
      useLayoutEffect(() => {
        if (interrupted) committed.push({
          text: (screen.getByLabelText("编辑开发计划") as HTMLTextAreaElement).value,
          disabled: (screen.getByRole("button", { name: "执行最终计划" }) as HTMLButtonElement).disabled,
        });
      }, [interrupted]);
      return <EditablePlan scope="task" initial={interrupted ? "" : "Previous generated plan"}
        interrupted={interrupted} busy={false} onExecute={vi.fn()} onSave={vi.fn()} />;
    }
    const view = render(<Panel interrupted={false} />);
    view.rerender(<Panel interrupted />);
    expect(committed).toEqual([{ text: "", disabled: true }]);
  });
});
