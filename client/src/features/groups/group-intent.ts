import rules from "./group-intent-rules.json";

const only = new RegExp(rules.only, "i");
const first = new RegExp(rules.first, "i");
const noWrite = new RegExp(rules.noWrite, "i");
const execute = new RegExp(rules.execute, "i");

/** Infer only whole-turn constraints. Local exclusions remain part of the prompt. */
export function groupDiscussionOnly(text: string): boolean {
  const clauses = text.replace(/```[\s\S]*?```|`[^`]*`/g, " ")
    .split(/[，,。；;！!？?\n]/)
    .map(clause => clause.replace(/@[^\s，,。；;：:！!？?]+\s*/g, " ").trim())
    .filter(Boolean);
  return !clauses.some(clause => execute.test(clause))
    && clauses.some(clause => only.test(clause) || first.test(clause) || noWrite.test(clause));
}
