/** Stable outbox identity shared by the chat and coding runtimes. */
export function codingQueueId(root: string, taskId?: string, sessionId?: string | null): string | null {
  return taskId && sessionId ? `coding:${JSON.stringify([root, taskId, sessionId])}` : null;
}

export function codingQueueOwner(key: string): { root: string; taskId: string; sessionId: string } | null {
  if (!key.startsWith("coding:")) return null;
  try {
    const owner: unknown = JSON.parse(key.slice(7));
    return Array.isArray(owner) && owner.length === 3
      && owner.every(value => typeof value === "string" && value.length > 0)
      ? { root: owner[0], taskId: owner[1], sessionId: owner[2] } : null;
  } catch { return null; }
}

export function queueBelongsToSession(key: string, sessionId: string): boolean {
  return key === sessionId || codingQueueOwner(key)?.sessionId === sessionId;
}
