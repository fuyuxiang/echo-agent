/**
 * 自包含纯 JS ZIP 读取器 —— 无外部依赖。
 *
 * 用途:为 FilePreview 的 docx/pptx/sheet 预览提供运行时 ZipReader(OOXML 是 zip)。
 * 支持:
 *  - 通过中央目录定位条目，兼容空目录和流式 ZIP 的 data descriptor
 *  - 解压 method 0(STORE,直接拷贝)与 method 8(DEFLATE,内含迷你 inflate)
 *  - UTF-8 文本读取(供 OOXML XML 提取)
 *
 * 无中央目录的本地文件头片段仅支持已知压缩大小；不会扫描压缩数据猜测边界。
 * 迷你 inflate 实现 raw DEFLATE(bit-stream + Huffman + LZ77),约 150 行,完整覆盖。
 */

/** 一个 zip 内的 entry。 */
export interface ZipEntry {
  name: string;
  /** 压缩方法:0=STORE,8=DEFLATE。 */
  method: number;
  /** 压缩数据在该 Uint8Array 中的起始偏移。 */
  dataOffset: number;
  /** 压缩数据长度。 */
  compressedSize: number;
}

export const MAX_ZIP_INPUT_BYTES = 8 * 1024 * 1024;
const MAX_ZIP_ENTRIES = 512;
const MAX_ENTRY_OUTPUT_BYTES = 4 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_BYTES = 16 * 1024 * 1024;

function validateEntryName(name: string): void {
  if (name.length > 4_096 || name.includes("\0") || name.startsWith("/")
      || name.replace(/\\/g, "/").split("/").includes("..")) {
    throw new Error("zip entry name is unsafe");
  }
}

/** The directory owns sizes when bit 3 leaves them absent from local headers. */
function centralDirectoryEntries(data: Uint8Array, dv: DataView): ZipEntry[] | null {
  // An EOCD has a 22-byte header and at most a 65535-byte comment.
  let end = -1;
  for (let offset = data.length - 22; offset >= Math.max(0, data.length - 22 - 65_535); offset--) {
    if (dv.getUint32(offset, true) === 0x06054b50
        && offset + 22 + dv.getUint16(offset + 20, true) === data.length) {
      end = offset;
      break;
    }
  }
  if (end < 0) return null;
  const count = dv.getUint16(end + 10, true);
  if (dv.getUint16(end + 4, true) !== 0 || dv.getUint16(end + 6, true) !== 0
      || dv.getUint16(end + 8, true) !== count) {
    throw new Error("multi-disk zip is unsupported");
  }
  if (count > MAX_ZIP_ENTRIES) throw new Error("zip contains too many entries");
  const size = dv.getUint32(end + 12, true);
  let offset = dv.getUint32(end + 16, true);
  const directoryStart = offset;
  const directoryEnd = offset + size;
  if (directoryEnd !== end) throw new Error("zip directory exceeds archive bounds");
  const entries: ZipEntry[] = [];
  for (let index = 0; index < count; index++) {
    if (offset + 46 > directoryEnd || dv.getUint32(offset, true) !== 0x02014b50) {
      throw new Error("zip directory entry exceeds archive bounds");
    }
    const flags = dv.getUint16(offset + 8, true);
    const method = dv.getUint16(offset + 10, true);
    const compressedSize = dv.getUint32(offset + 20, true);
    const nameLength = dv.getUint16(offset + 28, true);
    const next = offset + 46 + nameLength + dv.getUint16(offset + 30, true) + dv.getUint16(offset + 32, true);
    if (next > directoryEnd) throw new Error("zip entry header exceeds archive bounds");
    if ((flags & 1) !== 0) throw new Error("encrypted zip entries are unsupported");
    if (dv.getUint16(offset + 34, true) !== 0) throw new Error("multi-disk zip is unsupported");
    const name = utf8Decode(data.subarray(offset + 46, offset + 46 + nameLength));
    validateEntryName(name);
    const local = dv.getUint32(offset + 42, true);
    if (local + 30 > directoryStart || dv.getUint32(local, true) !== 0x04034b50) {
      throw new Error("zip local header exceeds archive bounds");
    }
    const localNameLength = dv.getUint16(local + 26, true);
    const dataOffset = local + 30 + localNameLength + dv.getUint16(local + 28, true);
    if (dataOffset + compressedSize > directoryStart) throw new Error("zip entry data exceeds archive bounds");
    if (dv.getUint16(local + 6, true) !== flags || dv.getUint16(local + 8, true) !== method
        || utf8Decode(data.subarray(local + 30, local + 30 + localNameLength)) !== name) {
      throw new Error("zip local header does not match directory");
    }
    entries.push({ name, method, dataOffset, compressedSize });
    offset = next;
  }
  if (offset !== directoryEnd) throw new Error("zip directory size does not match entries");
  return entries;
}

/**
 * 列出 zip 字节里的所有 entry(解析本地文件头)。
 * 每个本地文件头签名 0x04034b50(PK\x03\x04)。
 */
export function listZipEntries(data: Uint8Array): ZipEntry[] {
  if (data.byteLength > MAX_ZIP_INPUT_BYTES) {
    throw new Error("zip input exceeds 8MB preview limit");
  }
  const entries: ZipEntry[] = [];
  let off = 0;
  const dv = makeDataView(data);
  const directoryEntries = centralDirectoryEntries(data, dv);
  if (directoryEntries) return directoryEntries;
  while (off + 4 <= data.length) {
    const sig = dv.getUint32(off, true);
    if (sig !== 0x04034b50) break; // 不再是本地文件头
    if (off + 30 > data.length) throw new Error("zip entry header exceeds archive bounds");
    const flags = dv.getUint16(off + 6, true);
    if ((flags & 0x1) !== 0) throw new Error("encrypted zip entries are unsupported");
    if ((flags & 0x8) !== 0) throw new Error("streamed zip requires a central directory");
    const method = dv.getUint16(off + 8, true);
    const compressedSize = dv.getUint32(off + 18, true);
    const nameLen = dv.getUint16(off + 26, true);
    const extraLen = dv.getUint16(off + 28, true);
    const nameStart = off + 30;
    const nameEnd = nameStart + nameLen;
    const dataOffset = nameEnd + extraLen;
    if (nameEnd > data.length || dataOffset > data.length) {
      throw new Error("zip entry header exceeds archive bounds");
    }
    const name = utf8Decode(data.subarray(nameStart, nameEnd));
    const dataEnd = dataOffset + compressedSize;
    if (!Number.isSafeInteger(dataEnd) || dataEnd > data.length) {
      throw new Error("zip entry data exceeds archive bounds");
    }
    validateEntryName(name);
    if (entries.length >= MAX_ZIP_ENTRIES) {
      throw new Error("zip contains too many entries");
    }
    entries.push({ name, method, dataOffset, compressedSize });
    // Even an empty file/directory advances past its header and name.
    off = dataEnd;
  }
  return entries;
}

/** 解压单个 entry 的数据(STORE 或 DEFLATE)。 */
export function extractEntry(
  data: Uint8Array,
  entry: ZipEntry,
  maxOutputBytes = MAX_ENTRY_OUTPUT_BYTES,
): Uint8Array {
  if (
    entry.dataOffset < 0
    || entry.compressedSize < 0
    || entry.dataOffset + entry.compressedSize > data.length
  ) {
    throw new Error("zip entry data exceeds archive bounds");
  }
  const compressed = data.subarray(entry.dataOffset, entry.dataOffset + entry.compressedSize);
  if (entry.method === 0) {
    if (compressed.byteLength > maxOutputBytes) {
      throw new Error("zip entry exceeds preview output limit");
    }
    return compressed;
  }
  if (entry.method === 8) return inflate(compressed, maxOutputBytes); // DEFLATE
  throw new Error(`unsupported zip method: ${entry.method}`);
}

/**
 * 完整读取 zip:返回 name → 解压后字节 的映射。
 * 出错的 entry 被跳过(返回有效部分)。
 */
export function readZip(data: Uint8Array): Record<string, Uint8Array> {
  const out = Object.create(null) as Record<string, Uint8Array>;
  let totalOutputBytes = 0;
  for (const e of listZipEntries(data)) {
    try {
      const remaining = MAX_TOTAL_OUTPUT_BYTES - totalOutputBytes;
      if (remaining <= 0) break;
      const extracted = extractEntry(data, e, Math.min(MAX_ENTRY_OUTPUT_BYTES, remaining));
      totalOutputBytes += extracted.byteLength;
      out[e.name] = extracted;
    } catch {
      /* 跳过解压失败的 entry */
    }
  }
  return out;
}

/** 便捷:从 base64 字符串读取 zip(运行时把 data: URL 的 base64 部分传入)。 */
export function readZipFromBase64(base64: string): Record<string, Uint8Array> {
  return readZip(base64ToBytes(base64));
}

// ---------- 适配 doc-preview 的 ZipReader ----------

import type { ZipReader } from "./doc-preview";

/** 用 readZip 结果构造一个 doc-preview 的 ZipReader。 */
export function makeDocZipReader(files: Record<string, Uint8Array>): ZipReader {
  return {
    readText: (path) => {
      const bytes = files[path];
      return bytes ? utf8Decode(bytes) : null;
    },
    listEntries: () => Object.keys(files),
  };
}

/** 从 zip 字节构造 doc-preview ZipReader(完整解压一次)。 */
export function makeDocZipReaderFromBytes(data: Uint8Array): ZipReader {
  return makeDocZipReader(readZip(data));
}

// ---------- 工具:UTF-8 / base64 ----------

function utf8Decode(bytes: Uint8Array): string {
  return new TextDecoder("utf-8").decode(bytes);
}

function makeDataView(bytes: Uint8Array): DataView {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

function base64ToBytes(base64: string): Uint8Array {
  // 浏览器/atob 可用;Node 也有。清理 data: 前缀与空白。
  if (base64.length > Math.ceil(MAX_ZIP_INPUT_BYTES * 1.5) + 4_096) {
    throw new Error("base64 zip input exceeds preview limit");
  }
  const clean = base64.replace(/^data:[^;]*;base64,/, "").replace(/\s/g, "");
  if (clean.length > Math.ceil(MAX_ZIP_INPUT_BYTES * 4 / 3) + 4) {
    throw new Error("base64 zip input exceeds preview limit");
  }
  const bin = atob(clean);
  if (bin.length > MAX_ZIP_INPUT_BYTES) {
    throw new Error("zip input exceeds 8MB preview limit");
  }
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// ---------- 迷你 RAW INFLATE(DEFLATE 解压)----------
// 实现 RFC 1951:bit-stream + 固定/动态 Huffman + LZ77 回引。完整覆盖,无外部依赖。

function inflate(data: Uint8Array, maxOutputBytes: number): Uint8Array {
  const bs = new BitStream(data);
  const out: number[] = [];
  // 预填 32K 窗口足够(LZ77 距离 ≤ 32768)。
  let bfinal = 0;
  while (!bfinal) {
    bfinal = bs.readBits(1);
    const btype = bs.readBits(2);
    if (btype === 0) {
      // 无压缩块:对齐到字节,读 len/nlen,拷贝。
      bs.alignToByte();
      const len = bs.readAlignedUint16();
      const nlen = bs.readAlignedUint16();
      if ((len ^ 0xffff) !== nlen) throw new Error("deflate: invalid stored block length");
      if (out.length + len > maxOutputBytes) {
        throw new Error("deflate output exceeds preview limit");
      }
      for (let i = 0; i < len; i++) out.push(bs.readAlignedByte());
    } else if (btype === 1) {
      inflateBlock(bs, out, FIXED_LIT_TREE, FIXED_DIST_TREE, maxOutputBytes);
    } else if (btype === 2) {
      const { lit, dist } = readDynamicTrees(bs);
      inflateBlock(bs, out, lit, dist, maxOutputBytes);
    } else {
      throw new Error("invalid deflate block type 3");
    }
  }
  return new Uint8Array(out);
}

class BitStream {
  data: Uint8Array;
  pos = 0; // 位偏移(LSB-first 读:低位先出)
  bitBuf = 0;
  bitCount = 0;
  constructor(data: Uint8Array) {
    this.data = data;
  }
  /** LSB-first 读 n 位(数值字段、extra bits、Huffman 单位)。
   *  DEFLATE 的 bit-stream 整体是 LSB-first;Huffman 码虽 MSB-first 定义,
   *  但在流里是「逐位 LSB-first 读出后,把每位依次左移拼成码」。 */
  readBits(n: number): number {
    while (this.bitCount < n) {
      if (this.pos >> 3 >= this.data.length) throw new Error("deflate: unexpected EOF");
      this.bitBuf |= this.data[this.pos >> 3] << this.bitCount;
      this.pos += 8;
      this.bitCount += 8;
    }
    const val = this.bitBuf & ((1 << n) - 1);
    this.bitBuf >>= n;
    this.bitCount -= n;
    return val;
  }
  alignToByte() {
    // 丢弃残余缓冲位,对齐到字节边界。
    const consumed = this.bitCount;
    // bitCount 是缓冲里未消费的位;pos 已推进到「缓冲末尾字节之后」。
    // 丢弃缓冲即对齐到 pos(字节整数倍)。
    this.bitBuf = 0;
    this.bitCount = 0;
    void consumed;
  }
  readAlignedByte(): number {
    if (this.pos >> 3 >= this.data.length) throw new Error("deflate: unexpected EOF");
    const v = this.data[this.pos >> 3];
    this.pos += 8;
    return v;
  }
  readAlignedUint16(): number {
    const lo = this.readAlignedByte();
    const hi = this.readAlignedByte();
    return lo | (hi << 8);
  }
}

interface HuffTree {
  /** 按码长分组:sym → [length, code]。 */
  counts: number[]; // counts[len] = 该长度的码字数
  symbols: number[]; // 按长度升序、同长度按符号升序排列的符号
}

function buildHuffTree(lengths: number[]): HuffTree {
  const MAX = 16;
  const counts = new Array(MAX).fill(0);
  for (const l of lengths) counts[l]++;
  counts[0] = 0;
  const offsets: number[] = new Array(MAX).fill(0);
  for (let i = 1; i < MAX; i++) offsets[i] = offsets[i - 1] + counts[i - 1];
  const symbols: number[] = new Array(lengths.length).fill(0);
  for (let sym = 0; sym < lengths.length; sym++) {
    if (lengths[sym] !== 0) symbols[offsets[lengths[sym]]++] = sym;
  }
  return { counts, symbols };
}

function decodeHuff(bs: BitStream, tree: HuffTree): number {
  // 单缓冲 LSB-first:每位读出后左移拼成 canonical code(MSB-first)。
  let code = 0;
  let first = 0;
  let index = 0;
  for (let len = 1; len < 16; len++) {
    code = (code << 1) | bs.readBits(1);
    const count = tree.counts[len];
    if (code - first < count) return tree.symbols[index + (code - first)];
    index += count;
    first = (first + count) << 1;
  }
  throw new Error("deflate: invalid huffman code");
}

// 固定 Huffman 码长(lit 0–143:8, 144–255:9, 256–279:7, 280–287:8;dist 全 5)。
const FIXED_LIT_TREE = buildHuffTree(fixedLitLengths());
const FIXED_DIST_TREE = buildHuffTree(new Array(32).fill(5));
function fixedLitLengths(): number[] {
  const l = new Array(288);
  for (let i = 0; i < 144; i++) l[i] = 8;
  for (let i = 144; i < 256; i++) l[i] = 9;
  for (let i = 256; i < 280; i++) l[i] = 7;
  for (let i = 280; i < 288; i++) l[i] = 8;
  return l;
}

// 长度/距离基础表(RFC 1951)。
const LEN_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CODE_LEN_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
// code-length-code 16/17/18 的 extra bits 与 base:16 → 2 extra(3..6),17 → 3 extra(3..10),
// 18 → 7 extra(11..138)。base 用于 rep = readBits(extra) + base。
const CODE_LEN_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const CODE_LEN_BASE = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 3, 11];

function inflateBlock(
  bs: BitStream,
  out: number[],
  lit: HuffTree,
  dist: HuffTree,
  maxOutputBytes: number,
) {
  for (;;) {
    const sym = decodeHuff(bs, lit);
    if (sym < 256) {
      if (out.length >= maxOutputBytes) {
        throw new Error("deflate output exceeds preview limit");
      }
      out.push(sym);
    } else if (sym === 256) {
      return; // 块结束
    } else {
      const li = sym - 257;
      if (li < 0 || li >= LEN_BASE.length) throw new Error("deflate: invalid length symbol");
      const length = LEN_BASE[li] + (LEN_EXTRA[li] ? bs.readBits(LEN_EXTRA[li]) : 0);
      const dsym = decodeHuff(bs, dist);
      if (dsym < 0 || dsym >= DIST_BASE.length) throw new Error("deflate: invalid distance symbol");
      const distance = DIST_BASE[dsym] + (DIST_EXTRA[dsym] ? bs.readBits(DIST_EXTRA[dsym]) : 0);
      const start = out.length - distance;
      if (distance <= 0 || start < 0) throw new Error("deflate: invalid back-reference");
      if (out.length + length > maxOutputBytes) {
        throw new Error("deflate output exceeds preview limit");
      }
      for (let i = 0; i < length; i++) out.push(out[start + i]);
    }
  }
}

function readDynamicTrees(bs: BitStream): { lit: HuffTree; dist: HuffTree } {
  const hlit = bs.readBits(5) + 257;
  const hdist = bs.readBits(5) + 1;
  const hclen = bs.readBits(4) + 4;
  const codeLenLengths = new Array(19).fill(0);
  for (let i = 0; i < hclen; i++) codeLenLengths[CODE_LEN_ORDER[i]] = bs.readBits(3);
  const codeLenTree = buildHuffTree(codeLenLengths);
  const lengths: number[] = [];
  while (lengths.length < hlit + hdist) {
    const sym = decodeHuff(bs, codeLenTree);
    if (sym < 16) {
      lengths.push(sym);
    } else if (sym === 16) {
      if (lengths.length === 0) throw new Error("deflate: repeat code has no predecessor");
      const rep = bs.readBits(CODE_LEN_EXTRA[16]) + CODE_LEN_BASE[16];
      const prev = lengths[lengths.length - 1] ?? 0;
      for (let i = 0; i < rep; i++) lengths.push(prev);
    } else if (sym === 17) {
      const rep = bs.readBits(CODE_LEN_EXTRA[17]) + CODE_LEN_BASE[17];
      for (let i = 0; i < rep; i++) lengths.push(0);
    } else if (sym === 18) {
      const rep = bs.readBits(CODE_LEN_EXTRA[18]) + CODE_LEN_BASE[18];
      for (let i = 0; i < rep; i++) lengths.push(0);
    }
    if (lengths.length > hlit + hdist) {
      throw new Error("deflate: code-length sequence exceeds declared size");
    }
  }
  const litLengths = lengths.slice(0, hlit);
  const distLengths = lengths.slice(hlit, hlit + hdist);
  return { lit: buildHuffTree(litLengths), dist: buildHuffTree(distLengths) };
}
