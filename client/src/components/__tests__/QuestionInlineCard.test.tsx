import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { clientMocks } = vi.hoisted(() => ({
  clientMocks: {
    resolve: vi.fn(),
    list: vi.fn(),
    listen: vi.fn(),
    closed: undefined as undefined | ((event: { requestId: string; sessionId: string }) => void),
  },
}));

vi.mock("@/lib/agent-client", () => ({
  agentResolveQuestion: clientMocks.resolve,
  agentListPendingInteractions: clientMocks.list,
  onQuestionClosedEvent: clientMocks.listen,
}));

import { QuestionInlineCard } from "../QuestionInlineCard";
import { useQuestionStore, type QuestionRequest } from "@/stores/question-store";

const request: QuestionRequest = {
  requestId: "question-1",
  sessionId: "session-1",
  toolCallId: "tool-1",
  title: "Choose",
  mode: "plan",
  timeout: 2,
  questions: [
    {
      id: "q-1",
      question: "Framework?",
      multiSelect: true,
      options: [
        { id: "react", label: "React", description: "UI", preview: "preview" },
        { id: "vue", label: "Vue", description: "Progressive" },
      ],
    },
  ],
};

const pending = () => ({
  permissions: [],
  questions: [request],
  planApprovals: [],
  folderTrustRequests: [],
});

describe("QuestionInlineCard", () => {
  beforeEach(() => {
    vi.useRealTimers();
    clientMocks.resolve.mockReset();
    clientMocks.list.mockReset().mockResolvedValue(pending());
    clientMocks.listen.mockReset().mockImplementation(async (callback) => {
      clientMocks.closed = callback;
      return vi.fn();
    });
    clientMocks.closed = undefined;
    useQuestionStore.getState().clearAll();
    useQuestionStore.getState().request(request);
  });

  it("keeps a multi-select request retryable until the backend ACK is true", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      // This case tests failed ACK and retry, while the separate fake-timer
      // case tests the two-second expiry. Keep CI load out of its premise.
      useQuestionStore.getState().clearAll();
      useQuestionStore.getState().request({ ...request, timeout: 30 });
      clientMocks.resolve.mockResolvedValueOnce(false).mockResolvedValueOnce(true);
      render(<QuestionInlineCard sessionId="session-1" />);

      fireEvent.click(screen.getByRole("button", { name: /React/ }));
      fireEvent.click(screen.getByRole("button", { name: /Vue/ }));
      fireEvent.click(screen.getByRole("button", { name: "提交" }));

      expect(await screen.findByRole("alert")).toHaveTextContent("后端未找到该提问请求");
      expect(useQuestionStore.getState().queues["session-1"]).toHaveLength(1);
      expect(clientMocks.resolve).toHaveBeenLastCalledWith("question-1", {
        outcome: "accepted",
        answers: { "Framework?": ["React", "Vue"] },
        annotations: undefined,
      });
      expect(consoleError).toHaveBeenCalledWith("resolve question failed", expect.any(Error));

      fireEvent.click(screen.getByRole("button", { name: "提交" }));
      await waitFor(() => {
        expect(useQuestionStore.getState().queues["session-1"]).toHaveLength(0);
      });
    } finally {
      consoleError.mockRestore();
    }
  });

  it("disables every response when the propagated timeout expires", async () => {
    vi.useFakeTimers();
    render(<QuestionInlineCard sessionId="session-1" />);
    expect(screen.getByText("剩余 2 秒")).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(2_100);
    });

    expect(screen.getByText(/已超时/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "提交" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "取消" })).toBeDisabled();
  });

  it("removes the card when the backend closes the reverse request", async () => {
    render(<QuestionInlineCard sessionId="session-1" />);
    await waitFor(() => expect(clientMocks.closed).toBeTypeOf("function"));

    act(() => clientMocks.closed?.({ requestId: "question-1", sessionId: "session-1" }));
    expect(screen.queryByText("Framework?")).not.toBeInTheDocument();
  });

  it("shows one question at a time and retains answers when navigating before a single final submission", async () => {
    clientMocks.resolve.mockResolvedValue(true);
    useQuestionStore.getState().clearAll();
    useQuestionStore.getState().request({
      ...request,
      timeout: 1800,
      questions: [
        request.questions[0],
        {
          id: "q-2",
          question: "MVP scope?",
          multiSelect: false,
          options: [
            { id: "contacts", label: "Contacts", description: "Customer records" },
            { id: "pipeline", label: "Sales pipeline", description: "Opportunity stages" },
          ],
        },
      ],
    });

    render(<QuestionInlineCard sessionId="session-1" />);

    expect(screen.getByRole("status", { name: "第 1 题，共 2 题" })).toBeInTheDocument();
    expect(screen.getByText("Framework?")).toBeInTheDocument();
    expect(screen.queryByText("MVP scope?")).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "当前问题" })).toHaveAttribute("tabindex", "0");
    expect(screen.getByRole("button", { name: "上一题" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "下一步" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: /React/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "补充回答" }), { target: { value: "支持离线使用" } });
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(clientMocks.resolve).not.toHaveBeenCalled();
    expect(screen.getByText("MVP scope?")).toHaveFocus();
    expect(screen.queryByText("Framework?")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Contacts/ }));
    fireEvent.click(screen.getByRole("button", { name: "上一题" }));
    expect(screen.getByRole("button", { name: /React/ })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("textbox", { name: "补充回答" })).toHaveValue("支持离线使用");
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByRole("button", { name: /Contacts/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(screen.getByRole("button", { name: "提交" }));
    await waitFor(() => expect(clientMocks.resolve).toHaveBeenCalledTimes(1));
    expect(clientMocks.resolve).toHaveBeenCalledWith("question-1", {
      outcome: "accepted",
      answers: { "Framework?": ["React", "Other"], "MVP scope?": ["Contacts"] },
      annotations: { "Framework?": { notes: "支持离线使用" } },
    });
  });

  it("skips unanswered questions without inventing answers or discarding earlier selections", async () => {
    clientMocks.resolve.mockResolvedValue(true);
    useQuestionStore.getState().clearAll();
    useQuestionStore.getState().request({
      ...request, timeout: 1800,
      questions: [request.questions[0], { ...request.questions[0], id: "q-2", question: "Anything else?" }],
    });
    render(<QuestionInlineCard sessionId="session-1" />);
    fireEvent.click(screen.getByRole("button", { name: /React/ }));
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    fireEvent.click(screen.getByRole("button", { name: "跳过并提交" }));
    await waitFor(() => expect(clientMocks.resolve).toHaveBeenCalledTimes(1));
    expect(clientMocks.resolve).toHaveBeenCalledWith("question-1", {
      outcome: "accepted", answers: { "Framework?": ["React"] }, annotations: undefined,
    });
  });

  it("supports option number shortcuts without capturing numbers or IME confirmation in custom text", () => {
    render(<QuestionInlineCard sessionId="session-1" />);
    fireEvent.keyDown(screen.getByRole("region", { name: "当前问题" }), { key: "1" });
    expect(screen.getByRole("button", { name: /React/ })).toHaveAttribute("aria-pressed", "true");
    const input = screen.getByRole("textbox", { name: "补充回答" });
    fireEvent.keyDown(input, { key: "2" });
    fireEvent.keyDown(screen.getByRole("region", { name: "当前问题" }), { key: "2", isComposing: true });
    expect(screen.getByRole("button", { name: /Vue/ })).toHaveAttribute("aria-pressed", "false");
  });

  it("treats a single-choice custom answer as an alternative and preserves the option's original label and preview", async () => {
    clientMocks.resolve.mockResolvedValue(true);
    useQuestionStore.getState().clearAll();
    useQuestionStore.getState().request({
      ...request, timeout: 1800,
      questions: [{ ...request.questions[0], multiSelect: false,
        options: [{ label: "React（推荐）", description: "UI", preview: "preview" }] }],
    });
    render(<QuestionInlineCard sessionId="session-1" />);
    const option = screen.getByRole("button", { name: /React/ });
    fireEvent.click(option);
    fireEvent.change(screen.getByRole("textbox", { name: "自定义回答" }), { target: { value: "另一种方案" } });
    expect(option).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(option);
    expect(screen.getByRole("textbox", { name: "自定义回答" })).toHaveValue("");
    fireEvent.click(option);
    expect(option).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByRole("button", { name: "跳过" })).toBeInTheDocument();
    fireEvent.click(option);
    fireEvent.click(screen.getByRole("button", { name: "提交" }));
    await waitFor(() => expect(clientMocks.resolve).toHaveBeenCalledWith("question-1", {
      outcome: "accepted", answers: { "Framework?": ["React（推荐）"] },
      annotations: { "Framework?": { preview: "preview" } },
    }));
  });

  it.each([
    ["聊聊这些选择", "chat_about_this"],
    ["跳过访谈直接规划", "skip_interview"],
  ])("preserves partial answers for the plan action %s", async (label, outcome) => {
    clientMocks.resolve.mockResolvedValue(true);
    render(<QuestionInlineCard sessionId="session-1" />);
    fireEvent.click(screen.getByRole("button", { name: /React/ }));
    fireEvent.click(screen.getByRole("button", { name: label }));
    await waitFor(() => expect(clientMocks.resolve).toHaveBeenCalledExactlyOnceWith("question-1", {
      outcome, partialAnswers: { "Framework?": "React" },
    }));
  });

  it("does not submit while the user confirms a Chinese IME candidate", async () => {
    clientMocks.resolve.mockResolvedValue(true);
    render(<QuestionInlineCard sessionId="session-1" />);
    const input = screen.getByRole("textbox", { name: "补充回答" });
    fireEvent.change(input, { target: { value: "中文回答" } });
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(clientMocks.resolve).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(clientMocks.resolve).toHaveBeenCalledTimes(1));
  });

  it("does not put a late answer failure into another session's question", async () => {
    const next = { ...request, requestId: "question-2", sessionId: "session-2", title: "Second question" };
    clientMocks.list.mockResolvedValue({ ...pending(), questions: [request, next] });
    let rejectAnswer!: (cause: Error) => void;
    clientMocks.resolve.mockImplementation(() => new Promise((_resolve, reject) => { rejectAnswer = reject; }));
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const { rerender } = render(<QuestionInlineCard sessionId="session-1" />);
      fireEvent.click(screen.getByRole("button", { name: /React/ }));
      fireEvent.click(screen.getByRole("button", { name: "提交" }));
      act(() => useQuestionStore.getState().request(next));
      rerender(<QuestionInlineCard sessionId="session-2" />);
      await act(async () => rejectAnswer(new Error("旧请求失败")));
      expect(screen.getByRole("region", { name: "Second question" })).toBeInTheDocument();
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /React/ })).toBeEnabled();
    } finally {
      consoleError.mockRestore();
    }
  });
});
