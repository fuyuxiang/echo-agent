import { formatDistanceToNow, format, isValid } from "date-fns";
import { zhCN, enUS } from "date-fns/locale";
import i18n from "../i18n";

/**
 * Shared time formatting for the dashboard.
 *
 * Three problems this centralises:
 * - date-fns throws a RangeError on an Invalid Date, which takes down the whole
 *   page render. Every entry point here validates first and degrades to a dash.
 * - Timestamps were rendered inconsistently: raw ISO strings (Memory),
 *   toLocaleString following the *browser* locale rather than the UI language
 *   (Cron), and a hand-rolled `ts.slice(11, 19)` (Logs) that assumed a fixed ISO
 *   layout and silently dropped the date.
 * - The date-fns locale has to follow i18n, not the browser.
 */
function locale() {
  return i18n.resolvedLanguage === "en" ? enUS : zhCN;
}

function parse(value: string | number | null | undefined): Date | null {
  if (value === null || value === undefined || value === "") return null;
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  return isValid(date) ? date : null;
}

const PLACEHOLDER = "-";

/** "3 minutes ago" — for "last active"/"last rebuild" style fields. */
export function relativeTime(value: string | number | null | undefined, fallback = PLACEHOLDER): string {
  const date = parse(value);
  if (!date) return fallback;
  return formatDistanceToNow(date, { locale: locale(), addSuffix: true });
}

/** "2026-07-26 14:03" — for absolute timestamps like a cron next-run. */
export function dateTime(value: string | number | null | undefined, fallback = PLACEHOLDER): string {
  const date = parse(value);
  if (!date) return fallback;
  return format(date, "yyyy-MM-dd HH:mm", { locale: locale() });
}

/** "14:03:27" — compact form for log lines, where the date goes in a tooltip. */
export function timeOfDay(value: string | number | null | undefined, fallback = PLACEHOLDER): string {
  const date = parse(value);
  if (!date) return fallback;
  return format(date, "HH:mm:ss", { locale: locale() });
}

/** Full precision, for the `title` of an abbreviated timestamp. */
export function fullTimestamp(value: string | number | null | undefined, fallback = PLACEHOLDER): string {
  const date = parse(value);
  if (!date) return fallback;
  return format(date, "yyyy-MM-dd HH:mm:ss", { locale: locale() });
}
