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

Missing assets are only tolerated where a dashboard genuinely cannot be
expected. A *distributable* wheel or sdist with no dashboard is a broken
release, so those builds fail loudly instead of quietly shipping a
Dashboard-less package to PyPI.
"""
from __future__ import annotations

import os
from pathlib import Path

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class DashboardAssetsHook(BuildHookInterface):
    """Bundle the built Dashboard SPA at every stage of the build pipeline."""

    PLUGIN_NAME = "dashboard"

    def initialize(self, version, build_data):
        dashboard_src = Path(self.root) / "web" / "dist"
        if not dashboard_src.is_dir() or not (dashboard_src / "index.html").is_file():
            self._handle_missing_dashboard(version)
            return

        # The hook runs for every target (wheel, sdist, editable wheel).
        # For the wheel target we route the SPA to its runtime path inside
        # the package; for the sdist target we use the same mapping so that
        # a downstream wheel rebuild (e.g. ``python -m build`` or a user
        # ``pip install`` of the sdist) can still locate the artifact.
        if self.target_name in ("wheel", "sdist"):
            build_data.setdefault("force_include", {})[str(dashboard_src)] = (
                "echo_agent/_bundled/dashboard"
            )

    def _handle_missing_dashboard(self, version) -> None:
        """Skip or fail, depending on whether this build is distributable.

        Two build paths legitimately have no ``web/dist``:

        - ``pip install -e .`` (``version == "editable"``): a development
          checkout serves the dashboard from ``web/dist`` at runtime, so
          nothing needs bundling and the frontend may not be built yet.
        - a wheel rebuilt from an *extracted sdist*: the sdist already
          carries the SPA at ``echo_agent/_bundled/dashboard`` (that is
          where this hook put it), and does not ship ``web/`` at all, so
          the assets are present even though ``web/dist`` is not.

        Anything else is a real release built without the frontend.
        """
        if version == "editable":
            return

        prebundled = Path(self.root) / "echo_agent" / "_bundled" / "dashboard"
        if (prebundled / "index.html").is_file():
            return  # rebuilt from an sdist that already carries the SPA

        if os.environ.get("ECHO_ALLOW_NO_DASHBOARD") == "1":
            # Escape hatch for `pip install .` from a checkout where the
            # frontend was deliberately not built. The resulting package has
            # no Dashboard; never use it for a release.
            return

        raise RuntimeError(
            "Dashboard assets are missing: neither web/dist/index.html nor "
            "echo_agent/_bundled/dashboard/index.html exists, so this "
            f"{self.target_name} would ship without the Dashboard. Build the "
            "frontend first:\n"
            "    cd web && pnpm install --frozen-lockfile && pnpm build\n"
            "(scripts/publish.sh does this for you.)\n"
            "To build a Dashboard-less package on purpose, set "
            "ECHO_ALLOW_NO_DASHBOARD=1."
        )
