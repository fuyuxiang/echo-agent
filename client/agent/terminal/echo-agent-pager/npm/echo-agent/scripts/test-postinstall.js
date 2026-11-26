#!/usr/bin/env node
// Tests for the versioned-binary + symlink installation logic used by
// postinstall.js and the bin/echoagent trampoline.
//
// Run with:  node scripts/test-postinstall.js
//
// Uses only Node.js built-in modules (no test framework needed).

const fs = require('fs');
const path = require('path');
const os = require('os');
const zlib = require('zlib');
const assert = require('assert');

let passed = 0;
let failed = 0;

function test(name, fn) {
    try {
        fn();
        console.log(`  ✓ ${name}`);
        passed++;
    } catch (e) {
        console.error(`  ✗ ${name}`);
        console.error(`    ${e.message}`);
        failed++;
    }
}

function makeTmpDir() {
    return fs.mkdtempSync(path.join(os.tmpdir(), 'echo-agent-test-'));
}

function cleanup(dir) {
    fs.rmSync(dir, { recursive: true, force: true });
}

// ─── Extracted logic (mirrors postinstall.js and bin/echoagent exactly) ─────

/** Comparator: sort "<prefix>X.Y.Z" filenames by version, newest first. */
function byVersionDescending(prefix) {
    return (a, b) => {
        const pa = a.slice(prefix.length).split('.').map(Number);
        const pb = b.slice(prefix.length).split('.').map(Number);
        for (let i = 0; i < 3; i++) {
            if ((pa[i] || 0) !== (pb[i] || 0)) return (pb[i] || 0) - (pa[i] || 0);
        }
        return 0;
    };
}

/** Install a versioned binary + atomic symlink (same as postinstall.js). */
function installVersionedBinary(vendoredBinPath, version, canonicalDir) {
    const canonicalPath = path.join(canonicalDir, 'echoagent');
    fs.mkdirSync(canonicalDir, { recursive: true });

    const versionedName = `echo-agent-${version}`;
    const versionedPath = path.join(canonicalDir, versionedName);

    if (!fs.existsSync(versionedPath)) {
        const tmpPath = versionedPath + `.tmp.${process.pid}`;
        try {
            fs.copyFileSync(vendoredBinPath, tmpPath);
            fs.chmodSync(tmpPath, 0o755);
            fs.renameSync(tmpPath, versionedPath);
        } finally {
            try { fs.unlinkSync(tmpPath); } catch {}
        }
    }

    const tmpLink = canonicalPath + `.link.${process.pid}`;
    try { fs.unlinkSync(tmpLink); } catch {}
    fs.symlinkSync(versionedName, tmpLink);
    fs.renameSync(tmpLink, canonicalPath);

    return { canonicalPath, versionedPath, versionedName };
}

/** Cleanup old versioned binaries (same as postinstall.js). */
function cleanupOldVersions(canonicalDir, currentVersionedName) {
    const entries = fs.readdirSync(canonicalDir);
    const versionedBinaries = entries
        .filter(e => e.startsWith('echo-agent-') && !e.includes('.tmp.') && !e.includes('.link.') && e !== currentVersionedName)
        .sort(byVersionDescending('echo-agent-'));
    // Keep the most recent old version, remove anything older.
    for (const old of versionedBinaries.slice(1)) {
        try { fs.unlinkSync(path.join(canonicalDir, old)); } catch {}
    }
    return versionedBinaries;
}

/** EchoAgent bin dir resolution (mirrors postinstall.js and bin/echoagent). */
function resolveEchoAgentBinDir(env, homedir) {
    const echoagentHome = env.ECHO_AGENT_HOME ?? path.join(homedir, '.echo-agent');
    return path.join(echoagentHome, 'bin');
}

/** Materialize the vendored binary at destPath (mirrors writeVendorBinary). */
function writeVendorBinary(brPath, rawPath, destPath) {
    const tmp = destPath + `.tmp.${process.pid}`;
    try {
        if (fs.existsSync(brPath)) {
            fs.writeFileSync(tmp, zlib.brotliDecompressSync(fs.readFileSync(brPath)));
        } else if (fs.existsSync(rawPath)) {
            fs.copyFileSync(rawPath, tmp);
        } else {
            return false;
        }
        fs.chmodSync(tmp, 0o755);
        fs.renameSync(tmp, destPath);
        return true;
    } catch {
        return false;
    } finally {
        try { fs.unlinkSync(tmp); } catch {}
    }
}

/** Decompress a brotli payload into the canonical dir (mirrors installBinary). */
function installBinaryFromBrotli(brPath, version, canonicalDir) {
    fs.mkdirSync(canonicalDir, { recursive: true });
    const versionedName = `echo-agent-${version}`;
    const versionedPath = path.join(canonicalDir, versionedName);
    const canonicalPath = path.join(canonicalDir, 'echoagent');

    if (!fs.existsSync(versionedPath)) {
        const tmpPath = versionedPath + `.tmp.${process.pid}`;
        try {
            const decompressed = zlib.brotliDecompressSync(fs.readFileSync(brPath));
            fs.writeFileSync(tmpPath, decompressed);
            fs.chmodSync(tmpPath, 0o755);
            fs.renameSync(tmpPath, versionedPath);
        } finally {
            try { fs.unlinkSync(tmpPath); } catch {}
        }
    }

    const tmpLink = canonicalPath + `.link.${process.pid}`;
    try { fs.unlinkSync(tmpLink); } catch {}
    fs.symlinkSync(versionedName, tmpLink);
    fs.renameSync(tmpLink, canonicalPath);

    return { canonicalPath, versionedPath, versionedName };
}

/** Bootstrap canonical from vendored (same as bin/echoagent trampoline). */
function bootstrapCanonical(vendoredBinPath, version, canonicalDir) {
    const canonicalPath = path.join(canonicalDir, 'echoagent');
    try {
        fs.mkdirSync(canonicalDir, { recursive: true });
        const versionedName = `echo-agent-${version}`;
        const versionedPath = path.join(canonicalDir, versionedName);
        if (!fs.existsSync(versionedPath)) {
            const tmpPath = versionedPath + `.tmp.${process.pid}`;
            fs.copyFileSync(vendoredBinPath, tmpPath);
            fs.chmodSync(tmpPath, 0o755);
            fs.renameSync(tmpPath, versionedPath);
        }
        const tmpLink = canonicalPath + `.link.${process.pid}`;
        try { fs.unlinkSync(tmpLink); } catch {}
        fs.symlinkSync(versionedName, tmpLink);
        fs.renameSync(tmpLink, canonicalPath);
        return canonicalPath;
    } catch {
        return vendoredBinPath;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Install + Symlink Tests
// ═══════════════════════════════════════════════════════════════════════

console.log('install + symlink tests\n');

test('creates versioned binary and symlink on fresh install', () => {
    const dir = makeTmpDir();
    try {
        const vendored = path.join(dir, 'vendored-echo-agent');
        fs.writeFileSync(vendored, 'binary-content-v1');

        const binDir = path.join(dir, 'bin');
        const result = installVersionedBinary(vendored, '0.1.140', binDir);

        // Versioned file should exist
        assert.ok(fs.existsSync(result.versionedPath), 'versioned binary should exist');
        assert.strictEqual(fs.readFileSync(result.versionedPath, 'utf8'), 'binary-content-v1');

        // Canonical path should be a symlink
        const stat = fs.lstatSync(result.canonicalPath);
        assert.ok(stat.isSymbolicLink(), 'canonical path should be a symlink');

        // Symlink should point to the versioned name (relative)
        const target = fs.readlinkSync(result.canonicalPath);
        assert.strictEqual(target, 'echo-agent-0.1.140');

        // Reading through the symlink should return the binary content
        assert.strictEqual(fs.readFileSync(result.canonicalPath, 'utf8'), 'binary-content-v1');
    } finally {
        cleanup(dir);
    }
});

test('upgrade swaps symlink and preserves old binary', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');

        // Install v1
        const vendored_v1 = path.join(dir, 'vendored-v1');
        fs.writeFileSync(vendored_v1, 'v1-content');
        installVersionedBinary(vendored_v1, '0.1.140', binDir);

        // Install v2
        const vendored_v2 = path.join(dir, 'vendored-v2');
        fs.writeFileSync(vendored_v2, 'v2-content');
        const result = installVersionedBinary(vendored_v2, '0.1.141', binDir);

        // Symlink now points to v2
        assert.strictEqual(fs.readlinkSync(result.canonicalPath), 'echo-agent-0.1.141');
        assert.strictEqual(fs.readFileSync(result.canonicalPath, 'utf8'), 'v2-content');

        // Old v1 binary MUST still exist on disk (this is the key safety property)
        const oldBinary = path.join(binDir, 'echo-agent-0.1.140');
        assert.ok(fs.existsSync(oldBinary), 'old versioned binary must not be deleted');
        assert.strictEqual(fs.readFileSync(oldBinary, 'utf8'), 'v1-content');
    } finally {
        cleanup(dir);
    }
});

test('idempotent: reinstalling same version does not re-copy', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');

        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'original');
        installVersionedBinary(vendored, '0.1.140', binDir);

        // Modify vendored source (simulate npm replacing it)
        fs.writeFileSync(vendored, 'replaced-by-npm');

        // Re-run postinstall with same version
        installVersionedBinary(vendored, '0.1.140', binDir);

        // Versioned binary should NOT have been replaced (existsSync guard)
        const versionedPath = path.join(binDir, 'echo-agent-0.1.140');
        assert.strictEqual(fs.readFileSync(versionedPath, 'utf8'), 'original');
    } finally {
        cleanup(dir);
    }
});

test('symlink swap is atomic (no intermediate missing state)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const canonicalPath = path.join(binDir, 'echoagent');

        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'v1');
        installVersionedBinary(vendored, '0.1.140', binDir);
        assert.ok(fs.existsSync(canonicalPath), 'should exist after first install');

        // Upgrade
        fs.writeFileSync(vendored, 'v2');
        installVersionedBinary(vendored, '0.1.141', binDir);
        assert.ok(fs.existsSync(canonicalPath), 'should exist after upgrade');

        // No temp files left behind
        const entries = fs.readdirSync(binDir);
        const tempFiles = entries.filter(e => e.includes('.tmp.') || e.includes('.link.'));
        assert.strictEqual(tempFiles.length, 0, `temp files should be cleaned up, found: ${tempFiles}`);
    } finally {
        cleanup(dir);
    }
});

test('handles upgrade from old-style regular file to versioned symlink', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const canonicalPath = path.join(binDir, 'echoagent');
        fs.mkdirSync(binDir, { recursive: true });

        // Simulate old installation: echoagent is a regular file
        fs.writeFileSync(canonicalPath, 'old-style-binary');
        assert.ok(!fs.lstatSync(canonicalPath).isSymbolicLink(), 'should be regular file initially');

        // Run new-style install
        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'v2-content');
        installVersionedBinary(vendored, '0.1.141', binDir);

        // Should now be a symlink
        assert.ok(fs.lstatSync(canonicalPath).isSymbolicLink(), 'should be symlink after install');
        assert.strictEqual(fs.readFileSync(canonicalPath, 'utf8'), 'v2-content');
    } finally {
        cleanup(dir);
    }
});

test('handles broken symlink (target deleted externally)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // Create a broken symlink (points to a file that doesn't exist)
        const canonicalPath = path.join(binDir, 'echoagent');
        fs.symlinkSync('echo-agent-0.1.99', canonicalPath);
        assert.ok(!fs.existsSync(canonicalPath), 'broken symlink should not "exist"');

        // Install should work and fix the broken symlink
        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'fixed-content');
        const result = installVersionedBinary(vendored, '0.1.141', binDir);

        assert.ok(fs.existsSync(result.canonicalPath), 'symlink should now resolve');
        assert.strictEqual(fs.readFileSync(result.canonicalPath, 'utf8'), 'fixed-content');
    } finally {
        cleanup(dir);
    }
});

test('three sequential upgrades: v1 -> v2 -> v3 all coexist', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');

        fs.writeFileSync(vendored, 'content-v1');
        installVersionedBinary(vendored, '0.1.1', binDir);

        fs.writeFileSync(vendored, 'content-v2');
        installVersionedBinary(vendored, '0.1.2', binDir);

        fs.writeFileSync(vendored, 'content-v3');
        installVersionedBinary(vendored, '0.1.3', binDir);

        // Symlink points to latest
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echoagent')), 'echo-agent-0.1.3');

        // All three versioned binaries still exist (no cleanup yet)
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.1')));
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.2')));
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.3')));
    } finally {
        cleanup(dir);
    }
});

test('file permissions are preserved (0o755)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'binary');

        const result = installVersionedBinary(vendored, '0.1.140', binDir);

        const mode = fs.statSync(result.versionedPath).mode & 0o777;
        assert.strictEqual(mode, 0o755, `expected 0755, got ${mode.toString(8)}`);
    } finally {
        cleanup(dir);
    }
});

// ═══════════════════════════════════════════════════════════════════════
// Cleanup / Semver Sort Tests
// ═══════════════════════════════════════════════════════════════════════

console.log('\ncleanup + semver sort tests\n');

test('cleanup keeps N-1 version and removes older ones', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // Create three old versioned binaries
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.138'), 'v138');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.139'), 'v139');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.140'), 'v140');
        // echo-agent-0.1.141 is the current version (excluded from cleanup)
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'v141');

        cleanupOldVersions(binDir, 'echo-agent-0.1.141');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'current should exist');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')), 'N-1 should be kept');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.139')), 'N-2 should be removed');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.138')), 'N-3 should be removed');
    } finally {
        cleanup(dir);
    }
});

test('cleanup with only one old version keeps it', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.140'), 'v140');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'v141');

        cleanupOldVersions(binDir, 'echo-agent-0.1.141');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')), 'single old version should be kept');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'current should exist');
    } finally {
        cleanup(dir);
    }
});

test('cleanup with no old versions is a no-op', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // Only the current version exists
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'v141');

        cleanupOldVersions(binDir, 'echo-agent-0.1.141');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'current should still exist');
        const entries = fs.readdirSync(binDir).filter(e => e.startsWith('echo-agent-'));
        assert.strictEqual(entries.length, 1, 'should only have current version');
    } finally {
        cleanup(dir);
    }
});

test('cleanup ignores .tmp. and .link. files', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'current');
        // Leftover temp files from a crashed install
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.140.tmp.12345'), 'crashed-tmp');
        fs.writeFileSync(path.join(binDir, 'echoagent.link.12345'), 'crashed-link');

        cleanupOldVersions(binDir, 'echo-agent-0.1.141');

        // Temp files should not be touched by cleanup (they're filtered out)
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.140.tmp.12345')), 'tmp file should not be touched');
        assert.ok(fs.existsSync(path.join(binDir, 'echoagent.link.12345')), 'link file should not be touched');
    } finally {
        cleanup(dir);
    }
});

test('semver sort: 0.1.9 vs 0.1.10 (digit boundary)', () => {
    // Regression test: lexical sort puts '0.1.9' after '0.1.10' because '9' > '1'
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.8'), 'v8');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.9'), 'v9');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.10'), 'v10');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.11'), 'v11');

        cleanupOldVersions(binDir, 'echo-agent-0.1.11');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.11')), 'current should exist');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.10')), '0.1.10 should be kept (N-1)');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.9')), '0.1.9 should be removed');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.8')), '0.1.8 should be removed');
    } finally {
        cleanup(dir);
    }
});

test('semver sort: major version boundary (0.x vs 1.x)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        fs.writeFileSync(path.join(binDir, 'echo-agent-0.9.99'), 'old');
        fs.writeFileSync(path.join(binDir, 'echo-agent-1.0.0'), 'v1');
        fs.writeFileSync(path.join(binDir, 'echo-agent-1.0.1'), 'current');

        cleanupOldVersions(binDir, 'echo-agent-1.0.1');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-1.0.0')), '1.0.0 should be kept (N-1)');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.9.99')), '0.9.99 should be removed');
    } finally {
        cleanup(dir);
    }
});

test('semver sort: minor version boundary (0.1.x vs 0.2.x)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.999'), 'old');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.2.0'), 'v2');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.2.1'), 'current');

        cleanupOldVersions(binDir, 'echo-agent-0.2.1');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.2.0')), '0.2.0 should be kept (N-1)');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.999')), '0.1.999 should be removed');
    } finally {
        cleanup(dir);
    }
});

test('byVersionDescending: unit test comparator directly', () => {
    const input = ['echo-agent-0.1.9', 'echo-agent-0.1.10', 'echo-agent-0.1.2', 'echo-agent-1.0.0', 'echo-agent-0.2.0'];
    const sorted = [...input].sort(byVersionDescending('echo-agent-'));
    assert.deepStrictEqual(sorted, [
        'echo-agent-1.0.0',
        'echo-agent-0.2.0',
        'echo-agent-0.1.10',
        'echo-agent-0.1.9',
        'echo-agent-0.1.2',
    ]);
});

// ═══════════════════════════════════════════════════════════════════════
// Bootstrap (trampoline) Tests
// ═══════════════════════════════════════════════════════════════════════

console.log('\nbootstrap (trampoline) tests\n');

test('bootstrapCanonical creates versioned binary from vendored', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored-echo-agent');
        fs.writeFileSync(vendored, 'vendored-content');

        const result = bootstrapCanonical(vendored, '0.1.140', binDir);

        assert.strictEqual(result, path.join(binDir, 'echoagent'));
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')), 'versioned binary should exist');
        assert.ok(fs.lstatSync(result).isSymbolicLink(), 'canonical should be symlink');
        assert.strictEqual(fs.readFileSync(result, 'utf8'), 'vendored-content');
    } finally {
        cleanup(dir);
    }
});

test('bootstrapCanonical is idempotent', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored-echo-agent');
        fs.writeFileSync(vendored, 'original-content');

        bootstrapCanonical(vendored, '0.1.140', binDir);

        // Change vendored content (simulating npm update)
        fs.writeFileSync(vendored, 'npm-replaced-content');

        // Second bootstrap should not overwrite existing versioned binary
        const result = bootstrapCanonical(vendored, '0.1.140', binDir);

        assert.strictEqual(
            fs.readFileSync(path.join(binDir, 'echo-agent-0.1.140'), 'utf8'),
            'original-content',
            'should keep original, not npm-replaced version'
        );
    } finally {
        cleanup(dir);
    }
});

test('bootstrapCanonical returns vendored path on failure', () => {
    // If the canonical dir can't be created (e.g. permission denied),
    // bootstrap should gracefully fall back to the vendored binary.
    const dir = makeTmpDir();
    try {
        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'fallback');

        // Create a regular file where the dir should be — mkdirSync will fail.
        const blockerFile = path.join(dir, 'blocked');
        fs.writeFileSync(blockerFile, 'I am a file, not a directory');
        const impossibleDir = path.join(blockerFile, 'subdir');

        const result = bootstrapCanonical(vendored, '0.1.140', impossibleDir);

        assert.strictEqual(result, vendored, 'should fall back to vendored path');
    } finally {
        cleanup(dir);
    }
});

test('bootstrapCanonical works when canonical already exists (different version)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');

        // Install v1 via postinstall
        const vendored1 = path.join(dir, 'vendored-v1');
        fs.writeFileSync(vendored1, 'v1');
        installVersionedBinary(vendored1, '0.1.140', binDir);

        // Bootstrap with v2 (simulates trampoline running a newer vendored binary)
        const vendored2 = path.join(dir, 'vendored-v2');
        fs.writeFileSync(vendored2, 'v2');
        const result = bootstrapCanonical(vendored2, '0.1.141', binDir);

        assert.strictEqual(result, path.join(binDir, 'echoagent'));
        // Symlink should now point to v2
        assert.strictEqual(fs.readlinkSync(result), 'echo-agent-0.1.141');
        // v1 should still exist
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')), 'old version should still exist');
    } finally {
        cleanup(dir);
    }
});

// ═══════════════════════════════════════════════════════════════════════
// End-to-end Scenario Tests
// ═══════════════════════════════════════════════════════════════════════

console.log('\nend-to-end scenario tests\n');

test('full lifecycle: install, upgrade, cleanup', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');

        // v1: fresh install
        fs.writeFileSync(vendored, 'v1');
        installVersionedBinary(vendored, '0.1.140', binDir);

        // v2: upgrade
        fs.writeFileSync(vendored, 'v2');
        installVersionedBinary(vendored, '0.1.141', binDir);

        // v3: another upgrade
        fs.writeFileSync(vendored, 'v3');
        installVersionedBinary(vendored, '0.1.142', binDir);
        cleanupOldVersions(binDir, 'echo-agent-0.1.142');

        // Current (v3) + N-1 (v2) should exist; v1 removed
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.142')), 'v3 should exist');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'v2 should be kept (N-1)');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')), 'v1 should be removed');

        // Canonical symlink points to v3
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echoagent')), 'echo-agent-0.1.142');
        assert.strictEqual(fs.readFileSync(path.join(binDir, 'echoagent'), 'utf8'), 'v3');
    } finally {
        cleanup(dir);
    }
});

test('downgrade: installing older version than current', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');

        // Install v2 first
        fs.writeFileSync(vendored, 'v2');
        installVersionedBinary(vendored, '0.1.141', binDir);

        // Downgrade to v1
        fs.writeFileSync(vendored, 'v1');
        installVersionedBinary(vendored, '0.1.140', binDir);

        // Symlink should now point to v1
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echoagent')), 'echo-agent-0.1.140');
        assert.strictEqual(fs.readFileSync(path.join(binDir, 'echoagent'), 'utf8'), 'v1');

        // v2 should still exist (never delete old binaries during install)
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'v2 should still exist');
    } finally {
        cleanup(dir);
    }
});

test('non-echo-agent files in bin dir are not touched by cleanup', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // Non-echo-agent files
        fs.writeFileSync(path.join(binDir, 'other-tool'), 'should-stay');
        fs.writeFileSync(path.join(binDir, 'README.md'), 'should-stay');

        // EchoAgent versions
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.138'), 'old1');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.139'), 'old2');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.140'), 'current');

        cleanupOldVersions(binDir, 'echo-agent-0.1.140');

        assert.ok(fs.existsSync(path.join(binDir, 'other-tool')), 'non-echo-agent file should not be touched');
        assert.ok(fs.existsSync(path.join(binDir, 'README.md')), 'non-echo-agent file should not be touched');
    } finally {
        cleanup(dir);
    }
});

// ═══════════════════════════════════════════════════════════════════════
// echoagent vs echo-agent-pager Isolation Tests
// ═══════════════════════════════════════════════════════════════════════

console.log('\nechoagent vs echo-agent-pager isolation tests\n');

/**
 * Cleanup for a named binary (mirrors postinstall.js cleanupOldVersions).
 * Uses prefix + leading digit to avoid echo-agent-* matching echo-agent-pager-*.
 */
function cleanupOldVersionsNamed(canonicalDir, binName, version) {
    const prefix = `${binName}-`;
    const currentVersioned = `${binName}-${version}`;
    const entries = fs.readdirSync(canonicalDir);
    const versionedBinaries = entries
        .filter(e => {
            if (!e.startsWith(prefix)) return false;
            if (e.includes('.tmp.') || e.includes('.link.')) return false;
            if (e === currentVersioned) return false;
            const suffix = e.slice(prefix.length);
            return /^\d/.test(suffix);
        })
        .sort(byVersionDescending(prefix));
    for (const old of versionedBinaries.slice(1)) {
        try { fs.unlinkSync(path.join(canonicalDir, old)); } catch {}
    }
    return versionedBinaries;
}

/** Install a named binary (mirrors postinstall.js installBinary). */
function installNamedBinary(vendoredBinPath, binName, version, canonicalDir) {
    fs.mkdirSync(canonicalDir, { recursive: true });
    const versionedName = `${binName}-${version}`;
    const versionedPath = path.join(canonicalDir, versionedName);
    const canonicalPath = path.join(canonicalDir, binName);

    if (!fs.existsSync(versionedPath)) {
        const tmpPath = versionedPath + `.tmp.${process.pid}`;
        try {
            fs.copyFileSync(vendoredBinPath, tmpPath);
            fs.chmodSync(tmpPath, 0o755);
            fs.renameSync(tmpPath, versionedPath);
        } finally {
            try { fs.unlinkSync(tmpPath); } catch {}
        }
    }

    const tmpLink = canonicalPath + `.link.${process.pid}`;
    try { fs.unlinkSync(tmpLink); } catch {}
    fs.symlinkSync(versionedName, tmpLink);
    fs.renameSync(tmpLink, canonicalPath);

    return { canonicalPath, versionedPath, versionedName };
}

test('installing both echoagent and echo-agent-pager creates independent symlinks', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'echo-agent-binary');
        const vendoredPager = path.join(dir, 'vendored-pager');
        fs.writeFileSync(vendoredPager, 'pager-binary');

        installNamedBinary(vendored, 'echoagent', '0.1.141', binDir);
        installNamedBinary(vendoredPager, 'echo-agent-pager', '0.1.141', binDir);

        // Both symlinks exist and point to correct targets
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echoagent')), 'echo-agent-0.1.141');
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echo-agent-pager')), 'echo-agent-pager-0.1.141');

        // Both versioned files exist with correct content
        assert.strictEqual(fs.readFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'utf8'), 'echo-agent-binary');
        assert.strictEqual(fs.readFileSync(path.join(binDir, 'echo-agent-pager-0.1.141'), 'utf8'), 'pager-binary');
    } finally {
        cleanup(dir);
    }
});

test('cleanup of echo-agent-* does not remove echo-agent-pager-*', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // Old echoagent versions
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.138'), 'old-echo-agent-1');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.139'), 'old-echo-agent-2');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.140'), 'old-echo-agent-3');
        // Current echoagent
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'current-echo-agent');

        // echo-agent-pager versions (should not be touched)
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.138'), 'old-pager-1');
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.139'), 'old-pager-2');
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.141'), 'current-pager');

        cleanupOldVersionsNamed(binDir, 'echoagent', '0.1.141');

        // echoagent cleanup: current + N-1 kept, older removed
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'current echoagent should exist');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')), 'N-1 echoagent should be kept');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.139')), 'N-2 echoagent should be removed');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.138')), 'N-3 echoagent should be removed');

        // ALL echo-agent-pager versions must be untouched
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.138')), 'echo-agent-pager-0.1.138 must survive echoagent cleanup');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.139')), 'echo-agent-pager-0.1.139 must survive echoagent cleanup');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.141')), 'echo-agent-pager-0.1.141 must survive echoagent cleanup');
    } finally {
        cleanup(dir);
    }
});

test('cleanup of echo-agent-pager-* does not remove echo-agent-*', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // echoagent versions (should not be touched)
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.138'), 'old-echo-agent-1');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.139'), 'old-echo-agent-2');
        fs.writeFileSync(path.join(binDir, 'echo-agent-0.1.141'), 'current-echo-agent');

        // Old echo-agent-pager versions
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.138'), 'old-pager-1');
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.139'), 'old-pager-2');
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.140'), 'old-pager-3');
        // Current pager
        fs.writeFileSync(path.join(binDir, 'echo-agent-pager-0.1.141'), 'current-pager');

        cleanupOldVersionsNamed(binDir, 'echo-agent-pager', '0.1.141');

        // echo-agent-pager cleanup: current + N-1 kept, older removed
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.141')), 'current pager should exist');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.140')), 'N-1 pager should be kept');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.139')), 'N-2 pager should be removed');
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.138')), 'N-3 pager should be removed');

        // ALL echoagent versions must be untouched
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.138')), 'echo-agent-0.1.138 must survive pager cleanup');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.139')), 'echo-agent-0.1.139 must survive pager cleanup');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')), 'echo-agent-0.1.141 must survive pager cleanup');
    } finally {
        cleanup(dir);
    }
});

test('full dual-binary lifecycle: install, upgrade, cleanup both', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');
        const vendoredPager = path.join(dir, 'vendored-pager');

        // v1
        fs.writeFileSync(vendored, 'echo-agent-v1');
        fs.writeFileSync(vendoredPager, 'pager-v1');
        installNamedBinary(vendored, 'echoagent', '0.1.140', binDir);
        installNamedBinary(vendoredPager, 'echo-agent-pager', '0.1.140', binDir);

        // v2
        fs.writeFileSync(vendored, 'echo-agent-v2');
        fs.writeFileSync(vendoredPager, 'pager-v2');
        installNamedBinary(vendored, 'echoagent', '0.1.141', binDir);
        installNamedBinary(vendoredPager, 'echo-agent-pager', '0.1.141', binDir);

        // v3
        fs.writeFileSync(vendored, 'echo-agent-v3');
        fs.writeFileSync(vendoredPager, 'pager-v3');
        installNamedBinary(vendored, 'echoagent', '0.1.142', binDir);
        installNamedBinary(vendoredPager, 'echo-agent-pager', '0.1.142', binDir);

        // Cleanup both independently
        cleanupOldVersionsNamed(binDir, 'echoagent', '0.1.142');
        cleanupOldVersionsNamed(binDir, 'echo-agent-pager', '0.1.142');

        // Current + N-1 for each
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.142')));
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.141')));
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-0.1.140')));

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.142')));
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.141')));
        assert.ok(!fs.existsSync(path.join(binDir, 'echo-agent-pager-0.1.140')));

        // Symlinks correct
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echoagent')), 'echo-agent-0.1.142');
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echo-agent-pager')), 'echo-agent-pager-0.1.142');
    } finally {
        cleanup(dir);
    }
});

// ═══════════════════════════════════════════════════════════════════════
// macOS-only Pager Platform Split Tests
// ═══════════════════════════════════════════════════════════════════════

console.log('\nmacOS-only pager platform split tests\n');

test('echoagent installs normally regardless of platform key', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored-echo-agent');
        fs.writeFileSync(vendored, 'echo-agent-binary');

        for (const platform of ['darwin-arm64', 'linux-x64', 'linux-arm64']) {
            const result = installNamedBinary(vendored, 'echoagent', '0.1.150', binDir);
            assert.ok(fs.existsSync(result.versionedPath), `echoagent should install for ${platform}`);
            assert.strictEqual(fs.readlinkSync(result.canonicalPath), 'echo-agent-0.1.150');
        }
    } finally {
        cleanup(dir);
    }
});

test('echo-agent-pager installs when vendored binary exists (darwin-arm64 path)', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendoredPager = path.join(dir, 'vendored-pager');
        fs.writeFileSync(vendoredPager, 'pager-binary');

        const result = installNamedBinary(vendoredPager, 'echo-agent-pager', '0.1.150', binDir);
        assert.ok(fs.existsSync(result.versionedPath), 'pager versioned binary should exist');
        assert.strictEqual(fs.readlinkSync(result.canonicalPath), 'echo-agent-pager-0.1.150');
        assert.strictEqual(fs.readFileSync(result.canonicalPath, 'utf8'), 'pager-binary');
    } finally {
        cleanup(dir);
    }
});

test('Linux pager vendor files are not required for echoagent install', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendorBase = path.join(dir, 'vendor');

        // Only darwin-arm64 pager exists (mirrors npm tarball)
        fs.mkdirSync(path.join(vendorBase, 'darwin-arm64'), { recursive: true });
        fs.writeFileSync(path.join(vendorBase, 'darwin-arm64', 'echo-agent-pager'), 'mac-pager');

        // Linux pager vendor dirs exist but without pager binaries
        fs.mkdirSync(path.join(vendorBase, 'linux-x64'), { recursive: true });
        fs.mkdirSync(path.join(vendorBase, 'linux-arm64'), { recursive: true });

        // Verify no Linux pager binaries
        assert.ok(!fs.existsSync(path.join(vendorBase, 'linux-x64', 'echo-agent-pager')));
        assert.ok(!fs.existsSync(path.join(vendorBase, 'linux-arm64', 'echo-agent-pager')));

        // echoagent install should succeed independently
        const echoagentVendored = path.join(dir, 'vendored-echo-agent');
        fs.writeFileSync(echoagentVendored, 'echo-agent-linux');
        const result = installNamedBinary(echoagentVendored, 'echoagent', '0.1.150', binDir);
        assert.ok(fs.existsSync(result.versionedPath), 'echoagent should install without Linux pager');
    } finally {
        cleanup(dir);
    }
});

test('skipping pager install on Linux does not affect echoagent cleanup', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        const vendored = path.join(dir, 'vendored');

        // Install echoagent across two versions
        fs.writeFileSync(vendored, 'echo-agent-v1');
        installNamedBinary(vendored, 'echoagent', '0.1.149', binDir);
        fs.writeFileSync(vendored, 'echo-agent-v2');
        installNamedBinary(vendored, 'echoagent', '0.1.150', binDir);

        // Simulate Linux: only run echoagent cleanup, skip pager entirely
        cleanupOldVersionsNamed(binDir, 'echoagent', '0.1.150');

        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.150')), 'current echoagent should exist');
        assert.ok(fs.existsSync(path.join(binDir, 'echo-agent-0.1.149')), 'N-1 echoagent should be kept');
        assert.strictEqual(fs.readlinkSync(path.join(binDir, 'echoagent')), 'echo-agent-0.1.150');

        // No pager files should exist at all
        const entries = fs.readdirSync(binDir);
        const pagerEntries = entries.filter(e => e.includes('pager'));
        assert.strictEqual(pagerEntries.length, 0, 'no pager artifacts on Linux');
    } finally {
        cleanup(dir);
    }
});

test('canonical pager from non-npm install is preserved on Linux', () => {
    const dir = makeTmpDir();
    try {
        const binDir = path.join(dir, 'bin');
        fs.mkdirSync(binDir, { recursive: true });

        // Simulate pager installed by install-echo-agent.sh (not npm)
        const pagerVersioned = path.join(binDir, 'echo-agent-pager-0.1.150');
        fs.writeFileSync(pagerVersioned, 'installer-pager-binary');
        fs.chmodSync(pagerVersioned, 0o755);
        const pagerCanonical = path.join(binDir, 'echo-agent-pager');
        fs.symlinkSync('echo-agent-pager-0.1.150', pagerCanonical);

        // Run echo-agent-only install + cleanup (simulating Linux postinstall)
        const vendored = path.join(dir, 'vendored');
        fs.writeFileSync(vendored, 'echo-agent-binary');
        installNamedBinary(vendored, 'echoagent', '0.1.150', binDir);
        cleanupOldVersionsNamed(binDir, 'echoagent', '0.1.150');

        // Pager installed by other means must be untouched
        assert.ok(fs.existsSync(pagerCanonical), 'canonical pager should survive');
        assert.ok(fs.existsSync(pagerVersioned), 'versioned pager should survive');
        assert.strictEqual(fs.readlinkSync(pagerCanonical), 'echo-agent-pager-0.1.150');
    } finally {
        cleanup(dir);
    }
});

console.log('\nechoagent home + brotli install tests\n');

test('resolveEchoAgentBinDir honors $ECHO_AGENT_HOME, else falls back to <home>/.echo-agent/bin', () => {
    assert.strictEqual(
        resolveEchoAgentBinDir({ ECHO_AGENT_HOME: '/fast/local/.echo-agent' }, '/home/alice'),
        path.join('/fast/local/.echo-agent', 'bin'),
    );
    assert.strictEqual(
        resolveEchoAgentBinDir({}, '/home/alice'),
        path.join('/home/alice', '.echo-agent', 'bin'),
    );
    assert.strictEqual(resolveEchoAgentBinDir({ ECHO_AGENT_HOME: '' }, '/home/alice'), path.join('', 'bin'));
});

test('writeVendorBinary returns false (not true) when the destination cannot be written', () => {
    const dir = makeTmpDir();
    try {
        const brPath = path.join(dir, 'echoagent.br');
        fs.writeFileSync(brPath, zlib.brotliCompressSync(Buffer.from('binary')));

        // A non-empty directory at destPath makes the final rename fail.
        const dest = path.join(dir, 'dest');
        fs.mkdirSync(dest);
        fs.writeFileSync(path.join(dest, 'child'), 'x');

        assert.strictEqual(writeVendorBinary(brPath, path.join(dir, 'raw'), dest), false);
        assert.ok(!fs.existsSync(`${dest}.tmp.${process.pid}`), 'temp file is cleaned up on failure');
    } finally {
        cleanup(dir);
    }
});

test('decompresses brotli into the canonical dir without duplicating into node_modules', () => {
    const dir = makeTmpDir();
    try {
        const vendorBin = path.join(dir, 'node_modules', 'bin');
        fs.mkdirSync(vendorBin, { recursive: true });
        const brPath = path.join(vendorBin, 'echoagent.br');
        fs.writeFileSync(brPath, zlib.brotliCompressSync(Buffer.from('native-binary-bytes')));

        const binDir = path.join(dir, '.echo-agent', 'bin');
        const result = installBinaryFromBrotli(brPath, '0.1.220', binDir);

        assert.ok(fs.lstatSync(result.canonicalPath).isSymbolicLink());
        assert.strictEqual(fs.readFileSync(result.canonicalPath, 'utf8'), 'native-binary-bytes');
        assert.ok(!fs.existsSync(path.join(vendorBin, 'echoagent')), 'no uncompressed binary in node_modules');
        assert.ok(fs.existsSync(brPath), 'compressed .br payload is preserved');
    } finally {
        cleanup(dir);
    }
});

// ─── Summary ───────────────────────────────────────────────────────────

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed > 0 ? 1 : 0);
