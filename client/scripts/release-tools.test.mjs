import fs from "node:fs";
import crypto from "node:crypto";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

import { checkVersions, setVersion, validateVersion } from "./release-version.mjs";
import { verifySignatureMetadata } from "./verify-updater-signature.mjs";
import { checkInstallerVersion, readPeVersion } from "./inspect-windows-installer.mjs";

const temporaryDirectories = [];

function temporaryDirectory() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "echoagent-release-test-"));
  temporaryDirectories.push(directory);
  return directory;
}

function versionProject() {
  const root = fs.realpathSync(temporaryDirectory());
  fs.mkdirSync(path.join(root, "src-tauri"));
  fs.mkdirSync(path.join(root, "scripts"));
  fs.copyFileSync(fileURLToPath(new URL("./release-version.mjs", import.meta.url)), path.join(root, "scripts", "release-version.mjs"));
  fs.writeFileSync(path.join(root, "package.json"), '{\n  "version": "0.1.0"\n}\n');
  fs.writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), '{\n  "version": "0.1.0"\n}\n');
  fs.writeFileSync(path.join(root, "src-tauri", "Cargo.toml"), '[package]\nname = "echoagent"\nversion = "0.1.0"\n\n[dependencies]\n');
  fs.writeFileSync(path.join(root, "src-tauri", "Cargo.lock"), 'version = 4\n\n[[package]]\nname = "echoagent"\nversion = "0.1.0"\n');
  return root;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

describe("release version synchronization", () => {
  it("updates and checks every version source", () => {
    const root = versionProject();

    expect(setVersion("1.2.3-rc.1+build.7", root)).toBe("1.2.3-rc.1+build.7");
    expect(checkVersions(undefined, root)).toBe("1.2.3-rc.1+build.7");
    expect(setVersion("1.2.3-rc.1+build.7", root)).toBe("1.2.3-rc.1+build.7");
  });

  it("rejects invalid SemVer", () => {
    expect(() => validateVersion("01.2.3")).toThrow(/SemVer/);
  });

  it.each(["direct", "directory link"])("executes the CLI through a %s checkout", (kind) => {
    const root = versionProject();
    let checkout = root;
    if (kind === "directory link") {
      checkout = path.join(temporaryDirectory(), "Repo[1] with spaces");
      fs.symlinkSync(root, checkout, process.platform === "win32" ? "junction" : "dir");
    }
    const script = path.join(checkout, "scripts", "release-version.mjs");
    const run = (...args) => spawnSync(process.execPath, [script, ...args], {
      cwd: os.tmpdir(), encoding: "utf8", timeout: 10_000,
    });
    const initial = run("check");
    expect(initial.status, initial.stderr).toBe(0);
    expect(initial.stdout).toBe("0.1.0\n");

    const changed = run("set", "1.2.3-rc.1+build.7");
    expect(changed.status, changed.stderr).toBe(0);
    expect(changed.stdout).toBe("1.2.3-rc.1+build.7\n");
    expect(checkVersions(undefined, root)).toBe("1.2.3-rc.1+build.7");
    const invalid = run("validate", "01.2.3");
    expect(invalid.status).toBe(1);
    expect(invalid.stdout).toBe("");
    expect(invalid.stderr).toMatch(/SemVer/);

    fs.writeFileSync(path.join(root, "package.json"), '{"version":"1.2.4"}');
    const mismatch = run("check");
    expect(mismatch.status).toBe(1);
    expect(mismatch.stdout).toBe("");
    expect(mismatch.stderr).toMatch(/版本不一致/);
  });

  it("does not run the CLI when imported", () => {
    const root = versionProject();
    const moduleUrl = pathToFileURL(path.join(root, "scripts", "release-version.mjs")).href;
    const imported = spawnSync(process.execPath, ["--input-type=module", "-e", `await import(${JSON.stringify(moduleUrl)}); console.log('imported')`], {
      encoding: "utf8", timeout: 10_000,
    });
    expect(imported.status, imported.stderr).toBe(0);
    expect(imported.stdout).toBe("imported\n");
    expect(imported.stderr).toBe("");
  });
});

describe("updater signature metadata", () => {
  it("verifies key ID, file bytes and the trusted comment", () => {
    const root = temporaryDirectory();
    const artifact = path.join(root, "EchoAgent-v1.2.3-windows-x86_64-setup.exe");
    const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
    const rawPublicKey = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
    const keyId = Buffer.from([1, 2, 3, 4, 5, 6, 7, 8]);
    const publicPacket = Buffer.concat([Buffer.from("Ed"), keyId, rawPublicKey]);
    fs.mkdirSync(path.join(root, "src-tauri"));
    fs.writeFileSync(artifact, "installer");
    fs.writeFileSync(
      path.join(root, "src-tauri", "tauri.conf.json"),
      JSON.stringify({
        plugins: {
          updater: {
            pubkey: Buffer.from(
              `untrusted comment: test\n${publicPacket.toString("base64")}\n`,
            ).toString("base64"),
          },
        },
      }),
    );
    const digest = crypto.createHash("blake2b512").update("installer").digest();
    const fileSignature = crypto.sign(null, digest, privateKey);
    const signaturePacket = Buffer.concat([Buffer.from("ED"), keyId, fileSignature]);
    const trustedComment = `timestamp:1\tfile:${path.basename(artifact)}`;
    const globalSignature = crypto.sign(
      null,
      Buffer.concat([fileSignature, Buffer.from(trustedComment)]),
      privateKey,
    );
    const signatureLines = [
      "untrusted comment: test",
      signaturePacket.toString("base64"),
      `trusted comment: ${trustedComment}`,
      globalSignature.toString("base64"),
    ];
    fs.writeFileSync(`${artifact}.sig`, Buffer.from(signatureLines.join("\n")).toString("base64"));

    expect(verifySignatureMetadata(artifact, undefined, root)).toBe("0102030405060708");

    fs.appendFileSync(artifact, "tampered");
    expect(() => verifySignatureMetadata(artifact, undefined, root)).toThrow(/文件内容/);

    fs.writeFileSync(artifact, "installer");
    const wrongKeyPacket = Buffer.from(signaturePacket);
    wrongKeyPacket[2] ^= 0xff;
    signatureLines[1] = wrongKeyPacket.toString("base64");
    fs.writeFileSync(`${artifact}.sig`, Buffer.from(signatureLines.join("\n")).toString("base64"));
    expect(() => verifySignatureMetadata(artifact, undefined, root)).toThrow(/密钥 ID/);
  });
});

function peWithVersion(major, minor, patch) {
  const bytes = Buffer.alloc(0x600);
  bytes.writeUInt16LE(0x5a4d, 0);
  bytes.writeUInt32LE(0x80, 0x3c);
  bytes.writeUInt32LE(0x00004550, 0x80);
  bytes.writeUInt16LE(1, 0x86); // One .rsrc section.
  bytes.writeUInt16LE(0xe0, 0x94);
  bytes.writeUInt16LE(0x10b, 0x98);
  bytes.writeUInt32LE(0x1000, 0x98 + 96 + 16);
  bytes.writeUInt32LE(0x400, 0x98 + 96 + 20);
  const section = 0x98 + 0xe0;
  bytes.write(".rsrc", section);
  bytes.writeUInt32LE(0x400, section + 8);
  bytes.writeUInt32LE(0x1000, section + 12);
  bytes.writeUInt32LE(0x400, section + 16);
  bytes.writeUInt32LE(0x200, section + 20);
  bytes.writeUInt16LE(1, 0x200 + 14);
  bytes.writeUInt32LE(16, 0x210);
  bytes.writeUInt32LE(0x80000020, 0x214);
  bytes.writeUInt16LE(1, 0x220 + 14);
  bytes.writeUInt32LE(1, 0x230);
  bytes.writeUInt32LE(0x80000040, 0x234);
  bytes.writeUInt16LE(1, 0x240 + 14);
  bytes.writeUInt32LE(1033, 0x250);
  bytes.writeUInt32LE(0x60, 0x254);
  bytes.writeUInt32LE(0x1100, 0x260);
  bytes.writeUInt32LE(92, 0x264);
  bytes.writeUInt16LE(92, 0x300);
  bytes.writeUInt16LE(52, 0x302);
  bytes.write("VS_VERSION_INFO\0", 0x306, "utf16le");
  bytes.writeUInt32LE(0xfeef04bd, 0x328);
  bytes.writeUInt32LE((major << 16) | minor, 0x330);
  bytes.writeUInt32LE(patch << 16, 0x334);
  return bytes;
}

describe("Windows installer version gate", () => {
  it("reads VERSIONINFO and rejects a renamed older installer", () => {
    const root = temporaryDirectory();
    const installer = path.join(root, "EchoAgent-v1.2.4-windows-x86_64-setup.exe");
    const bytes = peWithVersion(1, 2, 3);
    expect(readPeVersion(bytes)).toEqual([1, 2, 3, 0]);
    fs.writeFileSync(installer, bytes);
    expect(() => checkInstallerVersion(installer, "1.2.4")).toThrow(/does not match/);
    expect(checkInstallerVersion(installer, "1.2.3")).toBe("1.2.3.0");
    expect(() => readPeVersion(bytes.subarray(0, 0x280))).toThrow(/truncated|outside/);
  });
});
