/** Keep derived titles aligned with the ledger's punctuation-insensitive policy. */
const normalized = (text: string) => text.toLowerCase().replace(/[^\p{Alphabetic}\p{Number}]/gu, "");

export const memoryTitleFromContent = (content: string) => [...content.trim()].slice(0, 60).join("");

export function isContentDerivedMemoryTitle(key: string, content: string): boolean {
  if (!key.trim()) return true;
  const title = normalized(key);
  return [content.trim(), memoryTitleFromContent(content), content.trim().slice(0, 60)]
    .some(value => value === key.trim() || !!title && normalized(value) === title);
}
