import { useEffect, useState } from "react";
import { providersList } from "@/lib/agent-client";
import { listenOrgModelsChanged } from "@/lib/org-client";
import { withoutRemovedModels } from "@/lib/retired-models";
import { useOrgSessionStore } from "@/stores/org-session-store";
import { PromptPredictionSettings } from "./PromptPredictionSettings";
import { connectionName, modelName } from "./model-settings/provider-presets";

type SuggestionModel = { id: string; label: string };
type ModelLoad =
  | { status: "loading" }
  | { status: "error" }
  | { status: "ready"; models: SuggestionModel[] };

export function PromptPredictionModelSettings() {
  const [load, setLoad] = useState<ModelLoad>({ status: "loading" });
  const [revision, setRevision] = useState(0);
  const session = useOrgSessionStore(state => state.session);
  const hydrated = useOrgSessionStore(state => state.hydrated);
  const boundary = useOrgSessionStore(state => state.boundaryRevision);
  const organizationExpired = Boolean(session?.requiresReauthentication) || (hydrated && !session?.loggedIn);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void listenOrgModelsChanged(() => setRevision(value => value + 1)).then(dispose => {
      if (active) unlisten = dispose;
      else dispose();
    }).catch(() => {});
    return () => { active = false; unlisten?.(); };
  }, []);

  useEffect(() => {
    let active = true;
    setLoad({ status: "loading" });
    void providersList().then(raw => {
      if (!active) return;
      const catalog = withoutRemovedModels(raw);
      const providers = new Map(catalog.providers.map(provider => [provider.id, provider]));
      const available = catalog.models.filter(model => {
        const provider = providers.get(model.providerId);
        return provider && provider.credentialConfigured !== false
          && !(organizationExpired && provider.source === "organization");
      });
      const models = available.map(model => {
        const name = modelName(model);
        const provider = providers.get(model.providerId);
        const duplicate = available.some(other => other.modelId !== model.modelId && modelName(other) === name);
        return { id: model.modelId, label: duplicate && provider ? `${name} · ${connectionName(provider)}` : name };
      });
      setLoad({ status: "ready", models });
    }).catch(() => { if (active) setLoad({ status: "error" }); });
    return () => { active = false; };
  }, [organizationExpired, boundary, revision]);

  return <PromptPredictionSettings
    models={load.status === "ready" ? load.models : undefined}
    onRetry={load.status === "error" ? () => setRevision(value => value + 1) : undefined} />;
}
