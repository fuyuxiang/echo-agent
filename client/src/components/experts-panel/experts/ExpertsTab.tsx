import { useCallback } from "react";
import type { AgentEntry } from "@/lib/types";
import { usePendingExpertStore } from "@/stores/pending-expert-store";
import { AssistantsPanel } from "../../AssistantsPanel";

interface Props {
  cwd?: string;
  pills: React.ReactNode;
  onGoHome?: () => void;
  onToast?: (message: string) => void;
  createExpertRequested?: boolean;
  onCreateExpertRequestHandled?: () => void;
}

/** Local experts are available immediately, including when no external catalog exists. */
export function ExpertsTab({ cwd, pills, onGoHome, onToast, createExpertRequested, onCreateExpertRequestHandled }: Props) {
  const setPendingExpert = usePendingExpertStore((state) => state.set);
  const handleUseLocal = useCallback((a: AgentEntry) => {
    // Extract body from raw.
    let body = a.raw || a.description || "";
    const trimmed = body.trimStart();
    if (trimmed.startsWith("---")) {
      const afterOpen = trimmed.indexOf("\n");
      if (afterOpen !== -1) {
        const rest = trimmed.slice(afterOpen + 1);
        const closeMatch = rest.match(/\n---\s*(\n|$)/);
        if (closeMatch && closeMatch.index !== undefined) {
          body = rest.slice(closeMatch.index + closeMatch[0].length).trim();
        }
      }
    }
    setPendingExpert({
      name: a.name,
      prompt: body,
      description: a.description || a.name,
      expertId: a.name,
      definitionPath: a.path,
      definitionRaw: a.raw,
      source: "local",
    });
    onGoHome?.();
  }, [setPendingExpert, onGoHome]);

  return (
    <div className="capability-tab">
      {pills && <header className="um-topbar"><div className="um-topbar-left">{pills}</div></header>}
      <AssistantsPanel cwd={cwd} embedded onUseAssistant={handleUseLocal} onToast={onToast}
        createExpertRequested={createExpertRequested} onCreateExpertRequestHandled={onCreateExpertRequestHandled} />
    </div>
  );
}
