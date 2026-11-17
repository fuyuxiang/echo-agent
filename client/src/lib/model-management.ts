import { createContext } from "react";
import type { ModelOption } from "@/components/ModelSelector";

/** Shared by every model picker, including project dialogs and coding tasks. */
export const ModelManagementContext = createContext<((modelId?: string) => void) | undefined>(undefined);
/** The same effective catalog used by the task composer; never invent model IDs. */
export const ModelCatalogContext = createContext<ModelOption[]>([]);
