import { Cpu } from "lucide-react";
import { modelSourceLabel, type PresentableModel } from "@/lib/model-presentation";
import { EchoMark } from "./EchoMark";

const providerInitials: Record<string, string> = {
  openai: "O", anthropic: "A", minimax: "M", deepseek: "D", qwen: "Q",
};

/** Provider initials are labels, not invented vendor logos. Compatible gateways
 * use the neutral connection symbol rather than the upstream API's brand. */
export function ModelIdentity({ model }: { model: PresentableModel & { providerKind?: string } }) {
  const builtin = modelSourceLabel(model) === "内置";
  const initial = model.providerKind ? providerInitials[model.providerKind] : undefined;
  return <span className={`model-identity${builtin ? " model-identity--echo" : ""}`} aria-hidden="true">
    {builtin ? <EchoMark /> : initial ? <span>{initial}</span> : <Cpu size={15} strokeWidth={1.6} />}
  </span>;
}
