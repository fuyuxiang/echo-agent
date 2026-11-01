# Lockfile maintenance

npm prunes optional-dependency metadata based on the host platform when it writes `package-lock.json`. On a glibc Linux host running `npm install`, two things typically get dropped:

1. **Platform-specific optional deps** such as `@esbuild/darwin-arm64`, `@parcel/watcher-linux-x64-musl`, `@nx/nx-win32-x64-msvc`, or nested optional deps like puppeteer's proxy packages. If missing, `npm ci` on the affected platform cannot resolve the package.
2. **`libc` fields** on Linux entries (`"libc": ["glibc"]` or `"libc": ["musl"]`). Without them, `npm ci` on Alpine or other musl-based images cannot pick the right binary.

Both npm 10 (Node 22) and npm 11 (Node 24) do this. CI then breaks on the affected platforms even when it was green before.

## Updating the lockfile

Do not run a bare `npm install` and commit the result. Use:

```sh
node scripts/npm-install-with-platforms.js
```

The script snapshots the committed lockfile, runs `npm install --include=optional`, and restores optional entries and `libc` fields that npm stripped.

Use **npm 11.21.1 or newer**, matching the repository's installation checks and CI, with Node 22.23.4+ (22.x) or 24.22.1+ (24.x). Older npm versions do not reliably install the security workspaces and overrides.

For a lock-only update, set `npm_config_package_lock_only=true` and `npm_config_ignore_scripts=true` when running the platform-preserving script. Review the resulting dependency changes before installing them with `npm ci`.

Security changes must reach both this lockfile and the desktop's production lock. From the desktop repository root, explicitly refresh the affected transitive packages, because a plain install can retain an older resolution after an override changes:

```sh
npm_config_package_lock_only=true npm_config_ignore_scripts=true npm_config_audit=false npm_config_fund=false node vendor/theia-platform/scripts/npm-install-with-platforms.js --update follow-redirects glob minimatch
node scripts/update-theia-runtime-lock.mjs --update follow-redirects glob minimatch
```

The source updater preserves platform metadata during targeted updates as well as ordinary installs. Verify the result after any npm command that rewrites the source lock. The audit retains raw scanner reports and exact integrity-pinned reviews for the legacy minimatch branches; modern glob/minimatch branches must use the security fixes. Never replace the callable minimatch 3/5 API with a modern object export for legacy archiver or glob consumers.

After running:

```sh
git diff --stat package-lock.json
node scripts/verify-lockfile-platforms.js
```

The verifier checks that known Linux entries still carry their `libc` field, and that `allowScripts` is in sync (see below). It also runs in CI, so a lockfile that lost them cannot be merged.

## Keeping `allowScripts` in sync

npm 12 does not run dependency lifecycle scripts unless the package is listed in the `allowScripts` allowlist in the root `package.json`. Adding, removing, or updating dependencies can therefore require an `allowScripts` change:

- A new dependency with an install script must be added with `true` if the script is required (native bindings, downloaded binaries) or `false` if it is cosmetic or unused. Recording it as `false` also keeps `npm install` free of warnings.
- A dependency that no longer has an install script must be removed from the list.

Missing `true` entries are easy to overlook because `npm ci` still succeeds — the failure only shows up later, when a native module is loaded or a build step needs its binary. `node scripts/verify-lockfile-platforms.js` cross-checks the allowlist against every lockfile entry with `hasInstallScript` and fails on unlisted or stale entries, so run it after changing dependencies as well.

## Outlook

Once upstream npm produces a stable, platform-agnostic lockfile format, most of this file can be deleted.
