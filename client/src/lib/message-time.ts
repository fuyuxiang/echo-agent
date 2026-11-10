import { useEffect, useState } from "react";

export function readMessageTimestamp(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= 8.64e15
    ? value : undefined;
}

function startOfLocalDay(value: number): number {
  return new Date(value).setHours(0, 0, 0, 0);
}

/** One clock per transcript. Refresh after sleep and when the local day rolls over. */
export function useMessageDay(): number {
  const [today, setToday] = useState(() => startOfLocalDay(Date.now()));
  useEffect(() => {
    let timer: number;
    const refresh = () => {
      window.clearTimeout(timer);
      const now = Date.now();
      setToday(startOfLocalDay(now));
      const nextDay = new Date(now).setHours(24, 0, 0, 0);
      timer = window.setTimeout(refresh, nextDay - now);
    };
    refresh();
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);
  return today;
}

const twoDigits = (value: number) => String(value).padStart(2, "0");

export function formatMessageTime(value: number | undefined, now = Date.now()) {
  const timestamp = readMessageTimestamp(value);
  if (timestamp === undefined) return;
  const date = new Date(timestamp);
  const today = new Date(now);
  const yesterday = new Date(today);
  yesterday.setDate(yesterday.getDate() - 1);
  const clock = `${twoDigits(date.getHours())}:${twoDigits(date.getMinutes())}`;
  const monthDay = `${date.getMonth() + 1}月${date.getDate()}日`;
  const fullDate = `${date.getFullYear()}年${monthDay}`;
  const dateLabel = date.toDateString() === today.toDateString() ? ""
    : date.toDateString() === yesterday.toDateString() ? "昨天"
    : date.getFullYear() === today.getFullYear() ? monthDay : fullDate;
  return {
    text: dateLabel ? `${dateLabel} ${clock}` : clock,
    exact: `${fullDate} ${clock}:${twoDigits(date.getSeconds())}`,
    iso: date.toISOString(),
  };
}
