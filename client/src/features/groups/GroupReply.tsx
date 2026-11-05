import { memo, useMemo } from "react";
import type { MarkdownConfig } from "@/components/markdown/Markdown";
import { MessageItem } from "@/components/MessageItem";
import type { ChatMessage } from "@/stores/session-store";
import type { GroupMember, GroupMessage } from "./group-client";

/** A group reply renders the public answer directly, independently of tool progress. */
export const GroupReply = memo(function GroupReply({ message, member, coordinator, conclusion, complete, config, onToast }: {
  message: GroupMessage; member?: GroupMember; coordinator: boolean; conclusion: boolean;
  complete: boolean; config: MarkdownConfig; onToast?: (text: string) => void;
}) {
  // Reuse copy, speech, export, links and attachment handling, while passing
  // only public text to the common renderer. Tool records stay in Work.
  const reply = useMemo<ChatMessage>(() => ({id:message.id,role:"assistant",complete,
    attachments:message.attachments,parts:[{kind:"text",text:message.text}]}),[message.id,message.text,message.attachments,complete]);
  const name = member?.name ?? "成员";
  return <div className={`group-reply-message${conclusion ? " group-reply-message--conclusion" : ""}`}>
    <div className="group-reply-message__identity"><span className="group-member__avatar" aria-hidden="true">{Array.from(name)[0]}</span><strong>{name}</strong>{coordinator && name !== "协调员" && <small>协调员</small>}{conclusion && <span className="group-conclusion-label">本次结论</span>}</div>
    <div className="group-reply-message__body"><MessageItem message={reply} streaming={!complete} waitingForModel={false} hideIdentity authorName={name} markdownConfig={config} cwd={config.cwd} onToast={onToast} /></div>
  </div>;
});
