import { createContext, useContext, useId, useLayoutEffect } from "react";

export interface SettingsDraftState {
  dirty: boolean;
  busy?: boolean;
  label?: string;
}

// Editors also run outside Settings. A missing provider deliberately does
// nothing; their own cancel/close protection continues to work normally.
export const SettingsDraftContext = createContext<(
  id: string, state: SettingsDraftState | null,
) => void>(() => {});

/** Report drafts and pending writes to the settings dialog's navigation guard. */
export function useSettingsDraft({ dirty, busy = false, label = "编辑内容" }: SettingsDraftState) {
  const report = useContext(SettingsDraftContext);
  const id = useId();
  useLayoutEffect(() => {
    report(id, { dirty, busy, label });
  }, [report, id, dirty, busy, label]);
  // Updating a draft must not briefly unregister an operation still in flight.
  useLayoutEffect(() => () => report(id, null), [report, id]);
}
