import { createContext } from "react";

/** Shared by every model picker, including project dialogs and coding tasks. */
export const ModelManagementContext = createContext<((modelId?: string) => void) | undefined>(undefined);
