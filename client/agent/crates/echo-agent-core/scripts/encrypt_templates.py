#!/usr/bin/env python3
"""Regenerate src/prompt/prompt_encrypted.rs from the templates/ directory.

Run from this crate directory:
    python3 scripts/encrypt_templates.py
"""

from pathlib import Path

SEEDS = {
    "BASE_PROMPT_ENC": 0x5A,
    "CODEX_PROMPT_ENC": 0x7B,
    "SUBAGENT_PROMPT_ENC": 0x3D,
}
TEMPLATES = {
    "BASE_PROMPT_ENC": "prompt.md",
    "CODEX_PROMPT_ENC": "apply_patch_prompt.md",
    "SUBAGENT_PROMPT_ENC": "subagent_prompt.md",
}

SCRIPT_DIR = Path(__file__).resolve().parent
CRATE_DIR = SCRIPT_DIR.parent
TEMPLATE_DIR = CRATE_DIR / "templates"
OUT_PATH = CRATE_DIR / "src" / "prompt" / "prompt_encrypted.rs"


def xor_encrypt(data: bytes, seed: int) -> bytes:
    return bytes(b ^ ((seed + i) & 0xFF) for i, b in enumerate(data))


def main():
    lines = [
        "// Auto-generated -- do not edit.",
        "// Regenerate: python3 scripts/encrypt_templates.py",
        "// XOR-encrypted prompt templates (key = position-dependent seed).",
        "",
    ]
    for const_name, filename in TEMPLATES.items():
        path = TEMPLATE_DIR / filename
        data = path.read_bytes()
        enc = xor_encrypt(data, SEEDS[const_name])
        arr = ", ".join(str(b) for b in enc)
        # `#[rustfmt::skip]` keeps the multi-KB byte array on a single line so
        # rustfmt does not reflow it across thousands of lines on every fmt run.
        lines.append("#[rustfmt::skip]")
        lines.append(f"pub(crate) const {const_name}: &[u8] = &[{arr}];")
        lines.append("")

    seeds_arr = ", ".join(f"0x{s:02X}" for s in SEEDS.values())
    lines.append(f"pub(crate) const PROMPT_SEEDS: [u8; {len(SEEDS)}] = [{seeds_arr}];")
    lines.append("")

    OUT_PATH.write_text("\n".join(lines))
    print(f"Wrote {OUT_PATH.relative_to(CRATE_DIR)}")


if __name__ == "__main__":
    main()
