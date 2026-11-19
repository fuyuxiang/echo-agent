import { useEffect, useState } from "react";
import { CapabilityPills, type CapabilityTab } from "./CapabilityHeader";
import { ExpertsTab } from "./experts/ExpertsTab";
import { SkillsTab } from "./skills/SkillsTab";
import { ConnectorsTab } from "./connectors/ConnectorsTab";

interface Props {
  cwd?: string;
  /** Navigate to the home page (after summoning an expert). */
  onGoHome?: () => void;
  onToast?: (message: string) => void;
  /** Slash navigation can open a specific capability tab directly. */
  initialTab?: CapabilityTab;
  /** The capability page owns navigation; standalone callers retain pills. */
  hideNavigation?: boolean;
  createExpertRequested?: boolean;
  onCreateExpertRequestHandled?: () => void;
  configureConnector?: boolean;
  initialConnectorName?: string;
}

/** 专家、技能与连接器的本地管理页面。 */
export function ExpertsPanel({ cwd, onGoHome, onToast, initialTab = "experts", hideNavigation = false, createExpertRequested, onCreateExpertRequestHandled, configureConnector, initialConnectorName }: Props) {
  const [tab, setTab] = useState<CapabilityTab>(initialTab);

  useEffect(() => setTab(initialTab), [initialTab]);

  const pills = hideNavigation ? null : <CapabilityPills active={tab} onChange={setTab} />;

  return (
    <div className={`capabilities-panel${hideNavigation ? " capabilities-panel--embedded" : ""}`}>
      {tab === "experts" && (
        <ExpertsTab cwd={cwd} pills={pills} onGoHome={onGoHome} onToast={onToast} createExpertRequested={createExpertRequested} onCreateExpertRequestHandled={onCreateExpertRequestHandled} />
      )}
      {tab === "skills" && <SkillsTab cwd={cwd} pills={pills} onToast={onToast} />}
      {tab === "connectors" && <ConnectorsTab pills={pills} onToast={onToast} initialEditing={configureConnector} initialServerName={initialConnectorName} />}
    </div>
  );
}
