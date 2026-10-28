from __future__ import annotations

from echo_agent.agent.inspection.policy import INSPECT_OK_SENTINEL
from echo_agent.agent.inspection.store import InspectItem


def build_inspection_prompt(due_items: list[InspectItem], state: dict) -> str:
    lines = [
        "这是一次主动巡检。请依次执行下列巡检项（用你的工具实际检查），",
        "只在发现「值得用户知道的变化或异常」时，主动调用发消息工具告知用户。",
        f"若一切正常、或与上次结论相比无实质变化，请只回复 {INSPECT_OK_SENTINEL} 且不要主动发消息。",
        "",
        "巡检项：",
    ]
    for item in due_items:
        last = state.get(item.name, {}).get("last_conclusion", "")
        lines.append(f"- 【{item.name}】{item.check}")
        if last:
            lines.append(f"  （上次结论：{last}）")
    return "\n".join(lines)
