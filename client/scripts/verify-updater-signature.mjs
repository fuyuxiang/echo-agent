import fs from "node:fs";
import crypto from "node:crypto";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = path.resolve(SCRIPT_DIR, "..");

function fail(message) {
  throw new Error(message);
}

function strictBase64(value, label) {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) {
    fail(`${label} 不是合法的 Base64`);
  }
  return Buffer.from(value, "base64");
}

function decodeMinisignContainer(value, label, expectedAlgorithm, expectedLength) {
  let decoded;
  try {
    decoded = strictBase64(value.trim(), label).toString("utf8");
  } catch (error) {
    fail(`${label} 不是合法的 Base64：${error.message}`);
  }
  const lines = decoded.trimEnd().split(/\r?\n/);
  if (lines.length < 2) {
    fail(`${label} 不是合法的 minisign 文本`);
  }
  let packet;
  try {
    packet = strictBase64(lines[1], `${label} 的 minisign 数据`);
  } catch (error) {
    fail(`${label} 的 minisign 数据无效：${error.message}`);
  }
  if (packet.length !== expectedLength || packet.subarray(0, 2).toString("ascii") !== expectedAlgorithm) {
    fail(`${label} 的 minisign 算法或数据长度无效`);
  }
  return { decoded, lines, keyId: packet.subarray(2, 10).toString("hex").toUpperCase() };
}

function blake2bFile(filePath) {
  const hash = crypto.createHash("blake2b512");
  const file = fs.openSync(filePath, "r");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let bytesRead;
    while ((bytesRead = fs.readSync(file, buffer, 0, buffer.length, null)) > 0) {
      hash.update(buffer.subarray(0, bytesRead));
    }
  } finally {
    fs.closeSync(file);
  }
  return hash.digest();
}

export function verifySignatureMetadata(
  artifactPath,
  signaturePath = `${artifactPath}.sig`,
  root = PROJECT_ROOT,
) {
  if (!fs.statSync(artifactPath, { throwIfNoEntry: false })?.isFile()) {
    fail(`更新文件不存在：${artifactPath}`);
  }
  if (!fs.statSync(signaturePath, { throwIfNoEntry: false })?.isFile()) {
    fail(`签名文件不存在：${signaturePath}`);
  }

  const config = JSON.parse(
    fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
  );
  const publicKey = config?.plugins?.updater?.pubkey;
  if (typeof publicKey !== "string" || publicKey.length === 0) {
    fail("src-tauri/tauri.conf.json 没有 updater 公钥");
  }

  const signatureValue = fs.readFileSync(signaturePath, "utf8").trim();
  if (/\s/.test(signatureValue)) {
    fail(`${signaturePath} 必须是 Tauri 清单可直接使用的单行 Base64`);
  }

  const publicMetadata = decodeMinisignContainer(publicKey, "updater 公钥", "Ed", 42);
  const signatureMetadata = decodeMinisignContainer(
    signatureValue,
    signaturePath,
    "ED",
    74,
  );
  if (publicMetadata.keyId !== signatureMetadata.keyId) {
    fail(
      `签名密钥 ID ${signatureMetadata.keyId} 与应用内置公钥 ID ${publicMetadata.keyId} 不一致`,
    );
  }

  const expectedFile = `file:${path.basename(artifactPath)}`;
  const trustedComment = signatureMetadata.lines.find((line) =>
    line.startsWith("trusted comment:"),
  );
  if (!trustedComment?.includes(expectedFile)) {
    fail(`${signaturePath} 的 trusted comment 与文件名 ${path.basename(artifactPath)} 不匹配`);
  }

  if (signatureMetadata.lines.length !== 4) {
    fail(`${signaturePath} 缺少 minisign 全局签名`);
  }
  const publicPacket = strictBase64(publicMetadata.lines[1], "updater 公钥数据");
  const signaturePacket = strictBase64(
    signatureMetadata.lines[1],
    `${signaturePath} 的签名数据`,
  );
  const globalSignature = strictBase64(
    signatureMetadata.lines[3],
    `${signaturePath} 的全局签名`,
  );
  if (globalSignature.length !== 64) {
    fail(`${signaturePath} 的全局签名长度无效`);
  }

  const spkiPrefix = Buffer.from("302a300506032b6570032100", "hex");
  const publicKeyObject = crypto.createPublicKey({
    key: Buffer.concat([spkiPrefix, publicPacket.subarray(10)]),
    format: "der",
    type: "spki",
  });
  if (
    !crypto.verify(
      null,
      blake2bFile(artifactPath),
      publicKeyObject,
      signaturePacket.subarray(10),
    )
  ) {
    fail(`${signaturePath} 无法验证 ${artifactPath} 的文件内容`);
  }

  const trustedCommentValue = trustedComment.slice("trusted comment: ".length);
  if (
    !crypto.verify(
      null,
      Buffer.concat([
        signaturePacket.subarray(10),
        Buffer.from(trustedCommentValue, "utf8"),
      ]),
      publicKeyObject,
      globalSignature,
    )
  ) {
    fail(`${signaturePath} 的 trusted comment 全局签名无效`);
  }

  return publicMetadata.keyId;
}

if (import.meta.main) {
  try {
    const [artifactPath, signaturePath, ...extra] = process.argv.slice(2);
    if (!artifactPath || extra.length > 0) {
      process.stderr.write(
        "用法：node scripts/verify-updater-signature.mjs <更新文件> [签名文件]\n",
      );
      process.exitCode = 2;
    } else {
      const keyId = verifySignatureMetadata(
        path.resolve(artifactPath),
        signaturePath ? path.resolve(signaturePath) : undefined,
      );
      process.stdout.write(`${keyId}\n`);
    }
  } catch (error) {
    process.stderr.write(`错误：${error.message}\n`);
    process.exitCode = 1;
  }
}
