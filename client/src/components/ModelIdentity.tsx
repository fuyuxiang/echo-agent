import { Cpu } from "lucide-react";
import { type PresentableModel } from "@/lib/model-presentation";

const providerInitials: Record<string, string> = {
  openai: "O", anthropic: "A", minimax: "M", deepseek: "D", qwen: "Q",
};

/** Provider initials are labels, not invented vendor logos. Compatible gateways
 * use the neutral connection symbol rather than the upstream API's brand. */
export function ModelIdentity({ model }: { model: PresentableModel & { providerKind?: string } }) {
  const initial = model.providerKind ? providerInitials[model.providerKind] : undefined;
  return <span className="model-identity" aria-hidden="true">
    {initial ? <span>{initial}</span> : <Cpu size={15} strokeWidth={1.6} />}
  </span>;
}
