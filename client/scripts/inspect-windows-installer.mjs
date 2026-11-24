import { readFileSync } from "node:fs";

// Read the fixed FileVersion from the PE VERSIONINFO resource. This runs on
// macOS too, where the release machine cannot use PowerShell's VersionInfo.
export function readPeVersion(bytes) {
  const requireRange = (offset, length) => {
    if (!Number.isSafeInteger(offset) || offset < 0 || offset + length > bytes.length) {
      throw new Error("Windows installer has a truncated PE resource");
    }
  };
  const u16 = (offset) => { requireRange(offset, 2); return bytes.readUInt16LE(offset); };
  const u32 = (offset) => { requireRange(offset, 4); return bytes.readUInt32LE(offset); };

  if (u16(0) !== 0x5a4d) throw new Error("Windows installer is not a PE file");
  const pe = u32(0x3c);
  if (u32(pe) !== 0x00004550) throw new Error("Windows installer has no PE header");
  const sectionCount = u16(pe + 6);
  const optionalSize = u16(pe + 20);
  const optional = pe + 24;
  const magic = u16(optional);
  if (magic !== 0x10b && magic !== 0x20b) throw new Error("Unsupported PE optional header");
  const directory = optional + (magic === 0x10b ? 96 : 112);
  if (directory + 24 > optional + optionalSize) throw new Error("PE resource directory is missing");
  const resourceRva = u32(directory + 16);
  const resourceSize = u32(directory + 20);
  if (!resourceRva || !resourceSize) throw new Error("Windows installer has no version resource");
  const sections = [];
  const sectionTable = optional + optionalSize;
  for (let index = 0; index < sectionCount; index++) {
    const offset = sectionTable + index * 40;
    requireRange(offset, 40);
    sections.push({
      virtualSize: u32(offset + 8),
      rva: u32(offset + 12),
      rawSize: u32(offset + 16),
      rawOffset: u32(offset + 20),
    });
  }
  const fileOffset = (rva, length) => {
    const section = sections.find(({ rva: start, virtualSize, rawSize }) =>
      rva >= start && rva - start + length <= Math.min(virtualSize || rawSize, rawSize));
    if (!section) throw new Error("PE resource points outside the installer");
    const offset = section.rawOffset + rva - section.rva;
    requireRange(offset, length);
    return offset;
  };
  const resourceBase = fileOffset(resourceRva, resourceSize);
  const resourceAt = (relative, length) => {
    if (relative + length > resourceSize) throw new Error("PE resource tree is truncated");
    const offset = resourceBase + relative;
    requireRange(offset, length);
    return offset;
  };
  const entries = (relative) => {
    const directoryOffset = resourceAt(relative, 16);
    const count = u16(directoryOffset + 12) + u16(directoryOffset + 14);
    if (count > 4096) throw new Error("PE resource tree is too large");
    resourceAt(relative + 16, count * 8);
    return Array.from({ length: count }, (_, index) => {
      const offset = directoryOffset + 16 + index * 8;
      return { name: u32(offset), target: u32(offset + 4) };
    });
  };
  const descend = (target) => {
    if (!(target & 0x80000000)) throw new Error("PE version resource has no directory");
    return entries(target & 0x7fffffff);
  };
  const versionType = entries(0).find(({ name }) => name === 16);
  if (!versionType) throw new Error("Windows installer has no VERSIONINFO resource");
  const names = descend(versionType.target);
  if (!names.length) throw new Error("PE version resource has no name");
  const languages = descend(names[0].target);
  if (!languages.length) throw new Error("PE version resource has no language");
  const dataTarget = languages[0].target;
  if (dataTarget & 0x80000000) throw new Error("PE version resource has no data");
  const dataEntry = resourceAt(dataTarget, 16);
  const valueSize = u32(dataEntry + 4);
  const valueStart = fileOffset(u32(dataEntry), valueSize);
  const valueEnd = valueStart + valueSize;
  const blockLength = u16(valueStart);
  const fixedLength = u16(valueStart + 2);
  if (blockLength > valueSize || fixedLength < 52) throw new Error("Invalid VERSIONINFO block");
  let cursor = valueStart + 6;
  let key = "";
  while (cursor + 2 <= valueEnd) {
    const char = u16(cursor);
    cursor += 2;
    if (!char) break;
    key += String.fromCharCode(char);
  }
  if (key !== "VS_VERSION_INFO") throw new Error("Invalid VERSIONINFO key");
  cursor = valueStart + ((cursor - valueStart + 3) & ~3);
  if (cursor + 52 > valueStart + blockLength || u32(cursor) !== 0xfeef04bd) {
    throw new Error("Invalid fixed FileVersion data");
  }
  const high = u32(cursor + 8);
  const low = u32(cursor + 12);
  return [high >>> 16, high & 0xffff, low >>> 16, low & 0xffff];
}

export function checkInstallerVersion(filePath, expectedVersion) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:[-+].*)?$/.test(expectedVersion)) {
    throw new Error(`Invalid expected version: ${expectedVersion}`);
  }
  const actual = readPeVersion(readFileSync(filePath));
  const expected = expectedVersion.split(/[-+]/, 1)[0].split(".").map(Number);
  if (actual.slice(0, 3).some((part, index) => part !== expected[index])) {
    throw new Error(`Windows installer FileVersion ${actual.join(".")} does not match ${expectedVersion}`);
  }
  return actual.join(".");
}

if (import.meta.main) {
  try {
    const [filePath, expectedVersion, ...extra] = process.argv.slice(2);
    if (!filePath || !expectedVersion || extra.length) {
      throw new Error("Usage: node scripts/inspect-windows-installer.mjs <installer.exe> <version>");
    }
    console.log(checkInstallerVersion(filePath, expectedVersion));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
