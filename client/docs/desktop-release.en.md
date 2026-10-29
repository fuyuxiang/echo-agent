# Desktop releases

Release targets are Windows x86_64, macOS Apple Silicon, and macOS Intel. Linux has native source validation but no official installer yet.

## Build and validate

1. Use a native machine or CI runner for each target. Install Node.js 22 or 24, pnpm 10, the required Rust toolchain, protoc, and the platform tools. Run `pnpm install --frozen-lockfile`.
2. Run `pnpm dist:win` on Windows x64 and `pnpm dist:mac` on each macOS architecture. Production packages need platform signing; the macOS build also requires a successful Gatekeeper assessment. `-AllowUnsignedPlatform` and `--allow-unsigned-platform` are for CI and local validation only.
3. The `desktop-validation` workflow runs these release scripts on all three native runners. It installs or mounts the package and checks the WebView, native IPC, and bundled Theia IDE startup. Confirm all three jobs pass before releasing.
4. On the controlled macOS release machine, run `scripts/prepare-update-artifacts.sh` with the Windows installer and both DMGs. It checks embedded versions, architectures, platform signatures, and all three updater signatures. Do not use `--unsigned` or `--allow-unsigned-platform` for a release.

Run `pnpm test:windows-packaging` on Windows for a quick regression check without compiling an installer. CI checks native warnings, failure exit codes, version output, and checkout paths containing brackets in both Windows PowerShell 5.1 and PowerShell 7. The build script stops on failure and restores the caller's working directory; native warnings with a zero exit code allow it to continue.

## Update server upgrade

Install the current `deploy/update-server/publish-update.py` and `deploy/update-server/nginx-location.conf` on the update server using `deploy/update-server/install.sh`. The installer copies existing single-target manifests into an initial generation. Nginx then reads the manifests through `stable/current`. Verify all existing update URLs still return their previous manifests before publishing a new version.

`stable/current` is a symlink to a complete manifest generation. The publisher copies and verifies all three update artifacts, writes the next generation, then atomically switches the symlink. A failed copy leaves all visible manifests unchanged. Previous generations remain available for diagnosis and rollback. Do not edit files behind `stable/current` directly.

## Publish

Run `bash scripts/publish-all-updates.sh --artifacts-dir release/v<VERSION> --dry-run`, then repeat without `--dry-run`. The script uploads all three updater artifacts and signatures; the server exposes their manifests together. Fetch each `stable/<target>.json` afterward and verify its version, signature, download URL, and file hash against `artifacts.json` and `SHA256SUMS`.

For a rollback, first verify the previous release files remain downloadable. A release owner can atomically point `stable/current` back to the previous generation and record the old and new generation names.
