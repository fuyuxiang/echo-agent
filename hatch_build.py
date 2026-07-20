"""Build hooks for the echo-agent package.

The web Dashboard SPA lives in ``web/dist/`` (gitignored build artifact).
It must end up at ``echo_agent/_bundled/dashboard`` inside the wheel that
users install from PyPI. We make that include *conditional* and *robust
across all build paths*:

- ``pip install -e .`` works in environments where the frontend has not
  been built yet (CI runners, fresh clones) — the hook silently skips.
- ``hatch build`` (used by ``scripts/publish.sh``) still bundles the
  dashboard because the hook sees the freshly-built ``web/dist``.
- ``python -m build`` and downstream ``pip install echo-agent.tar.gz``
  (sdist → wheel rebuild) also work, because the sdist target re-runs
  the same hook against its extracted copy of ``web/dist``.

The hook runs for both wheel and sdist targets. For the wheel target it
injects a ``force_include``. For the sdist target it injects an artifact
so the dashboard survives the sdist round-trip; the wheel hook then
re-activates the ``force_include`` when the sdist is later rebuilt.
"""
from __future__ import annotations

from pathlib import Path

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class DashboardAssetsHook(BuildHookInterface):
    """Bundle the built Dashboard SPA at every stage of the build pipeline."""

    PLUGIN_NAME = "dashboard"

    def initialize(self, version, build_data):  # noqa: ARG002 - hatch signature
        dashboard_src = Path(self.root) / "web" / "dist"
        if not dashboard_src.is_dir() or not (dashboard_src / "index.html").is_file():
            return  # partial build or no build yet — skip silently

        # The hook runs for every target (wheel, sdist, editable wheel).
        # For the wheel target we route the SPA to its runtime path inside
        # the package; for the sdist target we use the same mapping so that
        # a downstream wheel rebuild (e.g. ``python -m build`` or a user
        # ``pip install`` of the sdist) can still locate the artifact.
        if self.target_name in ("wheel", "sdist"):
            build_data.setdefault("force_include", {})[str(dashboard_src)] = (
                "echo_agent/_bundled/dashboard"
            )
