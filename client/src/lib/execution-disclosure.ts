// Message ids are stable while the renderer is running, including task switches.
// Keep the reader's choices and position here so the view can be remounted.
export type ReasoningDisclosureChoice = { open: boolean };
type ExecutionDisclosure = {
  open?: boolean;
  readingTop?: number;
  reasoning?: Map<string, ReasoningDisclosureChoice>;
};
const disclosures = new Map<string, ExecutionDisclosure>();
const MAX_DISCLOSURES = 1_000;

function keyFor(sessionId?: string, messageId?: string): string | null {
  return sessionId && messageId ? JSON.stringify([sessionId, messageId]) : null;
}

function updateDisclosure(key: string, update: ExecutionDisclosure): void {
  const next = { ...disclosures.get(key), ...update };
  disclosures.delete(key);
  if (next.open === undefined && next.readingTop === undefined && !next.reasoning?.size) return;
  disclosures.set(key, next);
  if (disclosures.size > MAX_DISCLOSURES) {
    const oldest = disclosures.keys().next().value;
    if (oldest !== undefined) disclosures.delete(oldest);
  }
}

export function getExecutionDisclosureChoice(
  sessionId?: string,
  messageId?: string,
): boolean | undefined {
  const key = keyFor(sessionId, messageId);
  return key ? disclosures.get(key)?.open : undefined;
}

export function rememberExecutionDisclosureChoice(
  sessionId: string | undefined,
  messageId: string | undefined,
  open: boolean,
): void {
  const key = keyFor(sessionId, messageId);
  if (key) updateDisclosure(key, open ? { open } : { open, reasoning: undefined });
}

export function forgetExecutionDisclosureChoice(
  sessionId?: string,
  messageId?: string,
): void {
  const key = keyFor(sessionId, messageId);
  if (key) disclosures.delete(key);
}

export function getExecutionReadingPosition(sessionId?: string, messageId?: string): number | undefined {
  const key = keyFor(sessionId, messageId);
  return key ? disclosures.get(key)?.readingTop : undefined;
}

export function rememberExecutionReadingPosition(
  sessionId: string | undefined,
  messageId: string | undefined,
  readingTop: number | undefined,
): void {
  const key = keyFor(sessionId, messageId);
  if (key) updateDisclosure(key, { readingTop });
}

export function getExecutionReasoningChoices(
  sessionId?: string,
  messageId?: string,
): ReadonlyMap<string, ReasoningDisclosureChoice> | undefined {
  const key = keyFor(sessionId, messageId);
  return key ? disclosures.get(key)?.reasoning : undefined;
}

export function rememberExecutionReasoningChoice(
  sessionId: string | undefined,
  messageId: string | undefined,
  rowId: string,
  choice: ReasoningDisclosureChoice,
): void {
  const key = keyFor(sessionId, messageId);
  if (!key) return;
  const reasoning = disclosures.get(key)?.reasoning ?? new Map<string, ReasoningDisclosureChoice>();
  if (choice.open) reasoning.set(rowId, choice);
  else reasoning.delete(rowId);
  updateDisclosure(key, { reasoning });
}
