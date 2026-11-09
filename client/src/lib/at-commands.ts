/**
 * @-mention token utilities, mirroring `slashTokenAtCursor` (`src/lib/slash-commands.ts`).
 *
 * Triggers when the user types "@" preceded by whitespace or at the start of
 * the buffer, and only consumes path-safe characters (no whitespace, no quotes)
 * up to the caret. The token is consumed verbatim by `AtMentionMenu` and
 * `replaceAtToken` to splice a chosen file / symbol reference back into the
 * textarea without disturbing the surrounding text.
 */

export interface AtToken {
  /** Inclusive start index of the "@" in the original text. */
  start: number;
  /** Exclusive end index (== caret position when the token is current). */
  end: number;
  /** Full token text including the leading "@" (e.g. "@src/foo"). */
  value: string;
  /** Lower-cased query without the leading "@". */
  query: string;
}

/**
 * Walk back from the caret looking for an "@" that opens an active mention.
 *
 * Returns `null` when:
 *  - there is no "@" before the caret,
 *  - the character immediately before "@" is non-whitespace (so we don't
 *    accidentally fire on `user@example.com`),
 *  - or the text between "@" and the caret contains whitespace / line breaks.
 */
export function atTokenAtCursor(text: string, cursor: number): AtToken | null {
  const safe = Math.max(0, Math.min(cursor, text.length));
  const before = text.slice(0, safe);
  const at = before.lastIndexOf("@");
  if (at < 0) return null;
  if (at > 0 && !/\s/.test(before[at - 1])) return null;
  const head = before.slice(at + 1);
  if (/[\s\n]/.test(head)) return null;
  if (!/^[\p{L}\p{N}._/\-:#]*$/u.test(head)) return null;
  return {
    start: at,
    end: safe,
    value: before.slice(at),
    query: head.toLowerCase(),
  };
}

/** A pasted or restored @ reference is also resolved against the task directory. */
export function hasRelativeWorkspaceMention(text: string): boolean {
  return [...text.matchAll(/(?:^|[\s([{])@([^\s]+)/gu)].some(([, reference]) => {
    const path = reference.replace(/^["'`]/, "");
    return path && !/^(?:[/\\]|[a-z]:[/\\]|[a-z][a-z0-9+.-]*:\/\/)/i.test(path);
  });
}

/**
 * Splice a chosen mention string (e.g. `"@src/foo.ts"`) into the buffer at the
 * active @-token's position. A trailing space is appended so the user can keep
 * typing naturally. Returns the new buffer + caret position, or `null` when
 * there is no active token (caller should treat that as a no-op).
 */
export function replaceAtToken(
  text: string,
  cursor: number,
  mention: string,
): { text: string; cursor: number } | null {
  const token = atTokenAtCursor(text, cursor);
  if (!token) return null;
  const replacement = `${mention} `;
  // Drop a single leading space if the user already typed one between the
  // token and the caret.
  const after = text.slice(token.end).replace(/^[ \t]/, "");
  const next = text.slice(0, token.start) + replacement + after;
  return { text: next, cursor: token.start + replacement.length };
}
