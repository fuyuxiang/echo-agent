# Release Process

Version release workflow for Echo Agent.

---

## Versioning

Echo Agent uses [SemVer](https://semver.org/). Currently in Beta (0.x.y).

## Steps

### 1. Prepare

- [ ] All CI checks pass
- [ ] Update `CHANGELOG.md`
- [ ] Bump version in `pyproject.toml`
- [ ] Verify docs match code

### 2. Build

```bash
cd web && pnpm install --frozen-lockfile && pnpm build && cd ..
test -f web/dist/index.html
hatch build
```

### 3. Verify

```bash
python -m venv /tmp/smoke
/tmp/smoke/bin/pip install dist/*.whl
/tmp/smoke/bin/echo-agent --version
```

### 4. Publish

```bash
hatch publish
```

### 5. Tag

```bash
git tag v0.3.x
git push origin v0.3.x
```

## Automation

`scripts/publish.sh` wraps steps 2-4.

## Checklist

- [ ] Dashboard built and included in wheel
- [ ] `echo_agent/_bundled/dashboard/index.html` in artifact
- [ ] `echo-agent --help` works after install
- [ ] Correct version
- [ ] CHANGELOG updated
- [ ] Git tag created
