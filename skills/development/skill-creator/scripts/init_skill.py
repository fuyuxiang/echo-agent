#!/usr/bin/env python3
"""
Skill Initializer - Creates a new skill from template

Usage:
    init_skill.py <skill-name> --path <path> [--resources scripts,references,assets] [--examples]

Examples:
    init_skill.py my-new-skill --path skills/public
    init_skill.py my-new-skill --path skills/public --resources scripts,references
    init_skill.py my-api-helper --path skills/private --resources scripts --examples
    init_skill.py custom-skill --path /custom/location
"""

import argparse
import re
import sys
from pathlib import Path

MAX_SKILL_NAME_LENGTH = 64
ALLOWED_RESOURCES = {"scripts", "references", "assets"}

SKILL_TEMPLATE = """---
name: {skill_name}
description: "Complete this description with what the skill does and when to use it. Include the trigger scenarios, file types, or tasks that should activate this skill."
---

# {skill_title}

## Overview

[Write 1-2 sentences explaining what this skill enables.]

## Structuring This Skill

[Choose the structure that best fits this skill's purpose. Common patterns:

**1. Workflow-Based** (best for sequential processes)
- Structure: ## Overview -> ## Workflow Decision Tree -> ## Step 1 -> ## Step 2...

**2. Task-Based** (best for tool collections)
- Structure: ## Overview -> ## Quick Start -> ## Task Category 1 -> ## Task Category 2...

**3. Reference/Guidelines** (best for standards or specifications)
- Structure: ## Overview -> ## Guidelines -> ## Specifications -> ## Usage...

**4. Capabilities-Based** (best for integrated systems)
- Structure: ## Overview -> ## Core Capabilities -> ### 1. Feature -> ### 2. Feature...

Delete this entire "Structuring This Skill" section when done - it's just guidance.]

## [Replace with the first main section based on the chosen structure]

[Add content here.]

## Resources (optional)

Create only the resource directories this skill actually needs. Delete this section if no resources are required.

### scripts/
Executable code (Python/Bash/etc.) that can be run directly to perform specific operations.

### references/
Documentation and reference material intended to be loaded into context to inform the agent's process and thinking.

### assets/
Files not intended to be loaded into context, but rather used within the output the agent produces.

---

**Not every skill requires all three types of resources.**
"""

EXAMPLE_SCRIPT = '''#!/usr/bin/env python3
"""
Example helper script for {skill_name}

Replace with actual implementation or delete if not needed.
"""

def main():
    print("This is an example script for {skill_name}")

if __name__ == "__main__":
    main()
'''

EXAMPLE_REFERENCE = """# Reference Documentation for {skill_title}

This is a placeholder for detailed reference documentation.
Replace with actual reference content or delete if not needed.

## When to Load This Reference

The agent should load this reference when:
- [Describe specific scenarios]
"""

EXAMPLE_ASSET_README = """# Assets for {skill_title}

Place files here that should be used in the agent's output (templates, images, fonts, etc.).
These files are NOT loaded into context — they are used directly in output.
"""


def normalize_skill_name(raw_name: str) -> str:
    name = raw_name.strip().lower()
    name = re.sub(r"[^a-z0-9]+", "-", name)
    name = name.strip("-")
    return name


def parse_resources(resources_str: str) -> list[str]:
    if not resources_str.strip():
        return []
    resources = [r.strip().lower() for r in resources_str.split(",") if r.strip()]
    invalid = [r for r in resources if r not in ALLOWED_RESOURCES]
    if invalid:
        print(f"[ERROR] Invalid resource types: {', '.join(invalid)}")
        print(f"   Allowed: {', '.join(sorted(ALLOWED_RESOURCES))}")
        sys.exit(1)
    return resources


def init_skill(skill_name: str, base_path: str, resources: list[str], examples: bool = False) -> bool:
    skill_title = skill_name.replace("-", " ").title()
    skill_path = Path(base_path) / skill_name

    if skill_path.exists():
        print(f"[ERROR] Directory already exists: {skill_path}")
        return False

    skill_path.mkdir(parents=True, exist_ok=True)

    skill_md = SKILL_TEMPLATE.format(skill_name=skill_name, skill_title=skill_title)
    (skill_path / "SKILL.md").write_text(skill_md, encoding="utf-8")
    print(f"  Created: {skill_path / 'SKILL.md'}")

    for resource in resources:
        resource_dir = skill_path / resource
        resource_dir.mkdir(exist_ok=True)
        print(f"  Created: {resource_dir}/")

        if examples:
            if resource == "scripts":
                example_file = resource_dir / f"example_{skill_name.replace('-', '_')}.py"
                example_file.write_text(
                    EXAMPLE_SCRIPT.format(skill_name=skill_name), encoding="utf-8"
                )
                print(f"  Created: {example_file}")
            elif resource == "references":
                example_file = resource_dir / "README.md"
                example_file.write_text(
                    EXAMPLE_REFERENCE.format(skill_title=skill_title), encoding="utf-8"
                )
                print(f"  Created: {example_file}")
            elif resource == "assets":
                example_file = resource_dir / "README.md"
                example_file.write_text(
                    EXAMPLE_ASSET_README.format(skill_title=skill_title), encoding="utf-8"
                )
                print(f"  Created: {example_file}")

    print(f"\n[OK] Skill '{skill_name}' initialized at: {skill_path}")
    return True


def main():
    parser = argparse.ArgumentParser(
        description="Initialize a new skill directory with a SKILL.md template.",
    )
    parser.add_argument("skill_name", help="Skill name (normalized to hyphen-case)")
    parser.add_argument("--path", required=True, help="Output directory for the skill")
    parser.add_argument(
        "--resources",
        default="",
        help="Comma-separated list: scripts,references,assets",
    )
    parser.add_argument(
        "--examples",
        action="store_true",
        help="Create example files inside the selected resource directories",
    )
    args = parser.parse_args()

    raw_skill_name = args.skill_name
    skill_name = normalize_skill_name(raw_skill_name)
    if not skill_name:
        print("[ERROR] Skill name must include at least one letter or digit.")
        sys.exit(1)
    if len(skill_name) > MAX_SKILL_NAME_LENGTH:
        print(
            f"[ERROR] Skill name '{skill_name}' is too long ({len(skill_name)} characters). "
            f"Maximum is {MAX_SKILL_NAME_LENGTH} characters."
        )
        sys.exit(1)
    if skill_name != raw_skill_name:
        print(f"Note: Normalized skill name from '{raw_skill_name}' to '{skill_name}'.")

    resources = parse_resources(args.resources)
    if args.examples and not resources:
        print("[ERROR] --examples requires --resources to be set.")
        sys.exit(1)

    path = args.path

    print(f"Initializing skill: {skill_name}")
    print(f"   Location: {path}")
    if resources:
        print(f"   Resources: {', '.join(resources)}")
        if args.examples:
            print("   Examples: enabled")
    else:
        print("   Resources: none (create as needed)")
    print()

    result = init_skill(skill_name, path, resources, args.examples)

    if result:
        sys.exit(0)
    else:
        sys.exit(1)


if __name__ == "__main__":
    main()
