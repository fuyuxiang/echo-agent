// Isolated native-event driver around the production chat, composer and queue.
import React, { useLayoutEffect } from "react";
import { ChatView } from "../../src/components/ChatView";
import { beginAgentTurn } from "../../src/lib/agent-turn";
import { useSessionStore } from "../../src/stores/session-store";
import { useSessionsStore } from "../../src/stores/sessions-store";
import { queueTerminalPolicy, useMessageQueueStore } from "../../src/stores/message-queue-store";

const sid = "queue-review";
const sent: { text: string; promptId: string }[] = [];
const pending = () => useMessageQueueStore.getState();
const chat = () => useSessionStore.getState();
function send(text: string, attachments: string[] = [], id?: string, replace = false): boolean {
  const promptId = crypto.randomUUID();
  if (id && !pending().claimById(sid, id, promptId)) return false;
  sent.push({ text, promptId });
  if (replace) chat().requestSendNow(promptId, sid);
  else beginAgentTurn({ sessionId: sid, promptId, promptText: text, displayText: text, attachments }, () => new Promise<void>(() => {}));
  return true;
}

export function QueueAdmissionReview() {
  useLayoutEffect(() => {
    const count = Number(new URLSearchParams(location.search).get("count")) || 1;
    sent.length = 0;
    useMessageQueueStore.setState({ queues: {} });
    chat().setSession(sid);
    chat().setMessages([]);
    chat().pushUser("请先处理上一轮工作", [], sid);
    chat().startStreaming(sid, "previous");
    useSessionsStore.setState({ currentSessionId: sid, independent: [{ sessionId: sid, title: "队列发送确认验收", cwd: "/review/EchoAgent", status: "working", currentModelId: "review/model" }] });
    for (let index = 0; index < count; index++) pending().enqueue(sid, index ? `后续要求 ${index}` : "1123", index ? [] : ["/review/需求.docx"]);
    Object.assign(window, { queueReview: {
      state: () => ({ queue: pending().getQueue(sid), sent, streaming: chat().streaming, sendNowPending: chat().sendNowPending }),
      acknowledge: (legacy = false) => {
        const current = sent.at(-1)!;
        chat().markComplete({ sessionId: sid, promptId: "previous", stopReason: "cancelled", cancelTrigger: "send_now" });
        chat().applyUpdate({ __sessionId: sid, sessionUpdate: "user_message_chunk", content: { type: "text", text: current.text },
          ...(legacy ? {} : { _meta: { promptId: current.promptId } }) });
        chat().applyUpdate({ __sessionId: sid, sessionUpdate: "agent_message_chunk", content: { type: "text", text: "已接收要求，正在执行。" }, _meta: { promptId: current.promptId } });
      },
      complete: (stopReason = "end_turn") => {
        const current = sent.at(-1)!;
        chat().applyUpdate({ __sessionId: sid, sessionUpdate: "turn_completed", prompt_id: current.promptId, stop_reason: stopReason });
        const completed = chat().markComplete({ sessionId: sid, promptId: current.promptId, stopReason });
        if (completed && queueTerminalPolicy(stopReason).autoAdvance) {
          const next = pending().getQueue(sid).find(item => item.status === "queued");
          if (next) send(next.text, next.attachments, next.id);
        }
      },
      reject: () => {
        const current = sent.at(-1)!;
        chat().rejectSendNow(current.promptId, sid);
        pending().holdSending(sid, current.promptId, false, "本次消息未被接收，请重试。");
      },
      replay: () => {
        const current = sent.at(-1)!;
        chat().applyUpdate({ __sessionId: sid, sessionUpdate: "user_message_chunk", content: { type: "text", text: current.text }, _meta: { promptId: current.promptId, isReplay: true } });
      },
    } });
  }, []);
  return <div className="app"><main className="app__main">
    <ChatView title="队列发送确认验收" apiReady modelId="review/model"
      models={[{ id: "review/model", label: "隔离验收模型", providerId: "review" }]}
      onSend={(text, attachments, id) => send(text, attachments, id)}
      onSendNow={(text, attachments, id) => send(text, attachments, id, true)}
      onCancel={() => true} />
  </main></div>;
}
