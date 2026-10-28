# Release Process

Run Runtime commands inside `echo-agent-runtime/`. Desktop releases keep using their independent workflow in `client/`.

## Preparation

- Update Runtime `CHANGELOG.md` and the version in `pyproject.toml`.
- Review bilingual documentation and model/storage compatibility notes.
- Require Python tests, Dashboard tests, documentation and package CI to pass.

## Build and verify

```bash
python -m pip install build
bash scripts/build.sh
```

This never publishes. It builds the Dashboard from its lockfile, performs the sdist → wheel build, creates a standalone Runtime archive and verifies all three artifacts:

- `dist/echo_agent-<version>-py3-none-any.whl`: installed Runtime, Dashboard, skills and configuration.
- `dist/echo_agent-<version>.tar.gz`: Python sdist containing prebuilt Dashboard assets, complete frontend sources and deployment templates.
- `dist/echo-agent-runtime-<version>.tar.gz`: extracts to `echo-agent-runtime/` with complete sources and prebuilt `web/dist/`; server installation needs no Node/pnpm.

Use `bash scripts/build.sh --prebuilt-dashboard` with existing trusted assets. Missing assets fail the build; never publish using `ECHO_ALLOW_NO_DASHBOARD`.

CI copies only Runtime outside the checkout, builds it, and installs both the wheel and the extracted Runtime source archive. Checks cover resources, version, CLI and real Gateway processes. Validate selected models, channels and external tools on the target server before release.

## Publish

```bash
python -m pip install twine
bash scripts/publish.sh
```

After building and verifying, the script uploads only the wheel/sdist to PyPI. Publish the standalone Runtime archive separately as a download attachment. Create the release tag and update download links.

## Documentation site

The repository root retains `docs/` and `mkdocs.yml`. Install with `pip install -e "./echo-agent-runtime[docs]"`, then generate references and run MkDocs from the repository root. The live site follows master rather than separate release versions.

Update both language versions of `docs/index.md` and `docs/operations/upgrade-migrations.md` when release versions or upgrade behavior change.
