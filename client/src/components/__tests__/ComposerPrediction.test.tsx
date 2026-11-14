import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Composer } from "../Composer";
import { useSessionsStore } from "@/stores/sessions-store";
import { saveDraftAttachments } from "@/lib/draft-attachments";

const prediction = { sessionId: "s1", turnId: "p1", generation: 1, text: "按这个方案实现并验证中文输入" };
const base = () => ({ streaming: false, apiReady: true, draftKey: "s1", prediction,
  onSend: vi.fn(), onCancel: vi.fn(), onDraftChange: vi.fn(), onDismissPrediction: vi.fn() });
beforeEach(() => { localStorage.clear(); useSessionsStore.setState({ drafts: {} }); });
afterEach(cleanup);

it("keeps ghost text out of the draft and accepts Tab without sending", () => {
  const props = base();
  render(<Composer {...props} />);
  const input = screen.getByRole("textbox");
  expect(screen.getByTestId("composer-prediction")).toHaveTextContent(prediction.text);
  expect(input).toHaveValue("");
  expect(props.onDraftChange).not.toHaveBeenCalled();
  fireEvent.keyDown(input, { key: "Enter" });
  expect(props.onSend).not.toHaveBeenCalled();
  fireEvent.keyDown(input, { key: "Tab" });
  expect(input).toHaveValue(prediction.text);
  expect(props.onDraftChange).toHaveBeenLastCalledWith(prediction.text);
  expect(props.onSend).not.toHaveBeenCalled();
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
  fireEvent.change(input, { target: { value: "修改后的指令" } });
  fireEvent.keyDown(input, { key: "Enter" });
  expect(props.onSend).toHaveBeenCalledWith("修改后的指令", []);
});

it.each(["existing-draft", "attachment", "running", "wrong-session"])("preserves %s when a prediction arrives", reason => {
  const props = base();
  if (reason === "attachment") saveDraftAttachments("s1", ["/work/用户文件.txt"]);
  render(<Composer {...props} draft={reason === "existing-draft" ? "现有草稿" : ""}
    streaming={reason === "running"} prediction={reason === "wrong-session" ? { ...prediction, sessionId: "s2" } : prediction} />);
  const input = screen.getByRole("textbox");
  fireEvent.keyDown(input, { key: "Tab" });
  expect(input).toHaveValue(reason === "existing-draft" ? "现有草稿" : "");
  expect(props.onSend).not.toHaveBeenCalled();
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
});

it("typing then deleting, or dismissing with Esc, cannot resurrect the old prediction", () => {
  const view = render(<Composer {...base()} />);
  const input = screen.getByRole("textbox");
  fireEvent.change(input, { target: { value: "自己的输入" } });
  fireEvent.change(input, { target: { value: "" } });
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
  view.rerender(<Composer {...base()} prediction={{ ...prediction, generation: 2 }} />);
  expect(screen.getByTestId("composer-prediction")).toBeInTheDocument();
  fireEvent.keyDown(input, { key: "Escape" });
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
  expect(input).toHaveValue("");
});

it("does not intercept modifier Tab or Chinese IME composition", () => {
  const props = base();
  render(<Composer {...props} />);
  const input = screen.getByRole("textbox");
  for (const modifier of ["shiftKey", "ctrlKey", "altKey", "metaKey"]) {
    fireEvent.keyDown(input, { key: "Tab", [modifier]: true });
    expect(input).toHaveValue("");
  }
  fireEvent.keyDown(input, { key: "Tab", keyCode: 229 });
  expect(input).toHaveValue("");
  fireEvent.compositionStart(input);
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
  fireEvent.keyDown(input, { key: "Tab", isComposing: true });
  fireEvent.compositionEnd(input);
  expect(input).toHaveValue("");
  expect(props.onSend).not.toHaveBeenCalled();
});

it("hides old predictions immediately when the session changes or a new turn starts", () => {
  const props = base();
  const view = render(<Composer {...props} />);
  view.rerender(<Composer {...props} draftKey="s2" />);
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
  view.rerender(<Composer {...props} streaming />);
  expect(screen.queryByTestId("composer-prediction")).toBeNull();
  expect(screen.getByRole("textbox")).toHaveValue("");
});

it("a deferred caret update after acceptance cannot move the caret in another draft", () => {
  const frames: FrameRequestCallback[] = [];
  const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation(callback => { frames.push(callback); return frames.length; });
  try {
    const props = base();
    const view = render(<Composer {...props} />);
    fireEvent.keyDown(screen.getByRole("textbox"), { key: "Tab" });
    view.rerender(<Composer {...props} draftKey="s2" draft="另一个会话的内容" />);
    const input = screen.getByRole("textbox") as HTMLTextAreaElement;
    input.setSelectionRange(1, 3);
    for (const callback of frames) callback(0);
    expect(input).toHaveValue("另一个会话的内容");
    expect([input.selectionStart, input.selectionEnd]).toEqual([1, 3]);
  } finally { raf.mockRestore(); }
});
