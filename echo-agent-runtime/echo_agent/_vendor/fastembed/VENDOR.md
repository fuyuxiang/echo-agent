# FastEmbed compatibility copy

This directory contains FastEmbed 0.9.0, licensed under Apache-2.0. The upstream
LICENSE and NOTICE are included unchanged.

- Upstream: https://github.com/qdrant/fastembed
- Official wheel: https://pypi.org/project/fastembed/0.9.0/
- Wheel SHA-256: `273d408edec8c0f161711d8f6e44e4a5b559d18e8edf6bf805415d55dc772846`
- Local version: `0.9.0+echo.1`

Upstream 0.9.0 requires Pillow `<13`, which selects Pillow 12.3.0 and blocks
the fixes for CVE-2026-112895, CVE-2026-112400, CVE-2026-112397,
CVE-2026-112896, CVE-2026-112897, CVE-2026-112395, CVE-2026-112404,
and CVE-2026-112899. Those fixes are available in Pillow 13.0.0.

The compatibility patch keeps upstream inference code and model/cache layouts.
It changes only:

1. Absolute internal imports from `fastembed` to `echo_agent._vendor.fastembed`,
   avoiding conflicts with an independently installed upstream package.
2. Version lookup in `__init__.py` to the local version, so a separate
   `fastembed` distribution is not required.
3. Dependency declarations: `pyproject.toml` carries upstream's runtime
   requirements, with Pillow changed to `>=13.0.0,<14`. Python 3.10 markers are
   omitted because Echo Agent requires Python 3.11 or newer.
4. An explanatory comment on the cache-only probe's exception handler in
   `common/model_management.py`: a failed probe falls through to the existing
   download/fallback paths, which report a final load failure. Behavior is unchanged.

Modified Python files carry an Echo Agent notice. To refresh the copy, verify
the official wheel hash, copy its `fastembed/` files and license notices, apply
the four changes above, and update the matching runtime requirements.

Remove this copy and restore the upstream dependency once a released FastEmbed
supports the patched Pillow version. Before updating, verify offline text
embedding, cross-encoder imports, skill image processing, and sdist/wheel contents.
