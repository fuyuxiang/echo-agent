// Minimal Compound File Binary (CFB / OLE2) reader — just enough to pull the
// `EncryptionInfo` and `EncryptedPackage` streams out of an encrypted OOXML
// file. A password-protected .docx is a CFB container (same shell as a legacy
// .doc), not a zip, so jszip can't see inside it.
//
// Layout: 512-byte header (4096-byte sectors when major version is 4) → FAT
// sectors → directory sectors → mini FAT → mini stream (for streams smaller
// than `miniCutoff`, `EncryptionInfo` among them).
const END_OF_CHAIN = 0xfffffffe;
const FREE_SECTOR = 0xffffffff;

export const CFB_SIGNATURE = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

export interface CompoundFileEntry {
	name: string;
	/** 1 = storage, 2 = stream, 5 = root */
	type: number;
	startSector: number;
	size: number;
}

export interface CompoundFile {
	entries: CompoundFileEntry[];
	/** Stream bytes by name, or null when the entry doesn't exist. */
	readStream(name: string): Uint8Array | null;
}

export function isCompoundFile(data: Uint8Array): boolean {
	if (data.length < 8) return false;
	for (let i = 0; i < CFB_SIGNATURE.length; i++) {
		if (data[i] !== CFB_SIGNATURE[i]) return false;
	}
	return true;
}

export function readCompoundFile(data: Uint8Array): CompoundFile | null {
	if (!isCompoundFile(data)) return null;
	const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
	if (data.length < 512) return null;

	const sectorShift = view.getUint16(30, true);
	const miniSectorShift = view.getUint16(32, true);
	if (sectorShift < 7 || sectorShift > 20 || miniSectorShift < 4 || miniSectorShift > 12) return null;
	const sectorSize = 1 << sectorShift;
	const miniSectorSize = 1 << miniSectorShift;
	const numberOfFatSectors = view.getUint32(44, true);
	const directoryStart = view.getUint32(48, true);
	const miniCutoff = view.getUint32(56, true);
	const miniFatStart = view.getUint32(60, true);
	const difatStart = view.getUint32(68, true);
	const numberOfDifatSectors = view.getUint32(72, true);

	const sectorOffset = (sector: number) => (sector + 1) * sectorSize;
	const hasSector = (sector: number) => sectorOffset(sector) + sectorSize <= data.length;

	// ── DIFAT → the list of FAT sectors (109 inline entries + a chained tail) ──
	const fatSectors: number[] = [];
	for (let i = 0; i < 109 && fatSectors.length < numberOfFatSectors; i++) {
		const sector = view.getUint32(76 + i * 4, true);
		if (sector >= FREE_SECTOR - 1 || !hasSector(sector)) break;
		fatSectors.push(sector);
	}
	let difatSector = difatStart;
	const perDifatSector = (sectorSize >> 2) - 1;
	for (let i = 0; i < numberOfDifatSectors && fatSectors.length < numberOfFatSectors; i++) {
		if (difatSector >= FREE_SECTOR - 1 || !hasSector(difatSector)) break;
		const base = sectorOffset(difatSector);
		for (let j = 0; j < perDifatSector && fatSectors.length < numberOfFatSectors; j++) {
			const sector = view.getUint32(base + j * 4, true);
			if (sector >= FREE_SECTOR - 1 || !hasSector(sector)) break;
			fatSectors.push(sector);
		}
		difatSector = view.getUint32(base + perDifatSector * 4, true);
	}

	const fat: number[] = [];
	for (const sector of fatSectors) {
		const base = sectorOffset(sector);
		for (let i = 0; i < sectorSize >> 2; i++) fat.push(view.getUint32(base + i * 4, true));
	}

	/** Sector numbers of a chain, following the given allocation table. */
	function chain(start: number, table: number[]): number[] {
		const out: number[] = [];
		let sector = start;
		const seen = new Set<number>();
		while (sector < FREE_SECTOR - 1 && !seen.has(sector)) {
			out.push(sector);
			seen.add(sector);
			sector = table[sector] ?? END_OF_CHAIN;
			if (out.length > table.length + 1) break;
		}
		return out;
	}

	function readChain(
		start: number,
		size: number,
		table: number[] = fat,
		sectorBytes = sectorSize,
		source: Uint8Array = data,
		at: (sector: number) => number = sectorOffset,
	): Uint8Array {
		const sectors = chain(start, table);
		const out = new Uint8Array(sectors.length * sectorBytes);
		sectors.forEach((sector, i) => out.set(source.subarray(at(sector), at(sector) + sectorBytes), i * sectorBytes));
		return out.subarray(0, Math.min(size, out.length));
	}

	// ── Directory ──
	const directoryBytes = readChain(directoryStart, Number.MAX_SAFE_INTEGER);
	const entries: CompoundFileEntry[] = [];
	for (let offset = 0; offset + 128 <= directoryBytes.length; offset += 128) {
		const entry = new DataView(directoryBytes.buffer, directoryBytes.byteOffset + offset, 128);
		const nameLength = entry.getUint16(64, true);
		const type = entry.getUint8(66);
		if (!nameLength || !type) continue;
		let name = '';
		for (let i = 0; i + 1 < nameLength - 1; i += 2) name += String.fromCharCode(entry.getUint16(i, true));
		entries.push({
			name,
			type,
			startSector: entry.getUint32(116, true),
			size: Number(entry.getBigUint64(120, true)),
		});
	}

	const root = entries.find((entry) => entry.type === 5);
	if (!root) return null;

	// ── Mini FAT + mini stream (root entry's own stream) ──
	const miniFat: number[] = [];
	for (const sector of chain(miniFatStart, fat)) {
		const base = sectorOffset(sector);
		for (let i = 0; i < sectorSize >> 2; i++) miniFat.push(view.getUint32(base + i * 4, true));
	}
	const miniStream = readChain(root.startSector, root.size);
	const readMiniStream = (start: number, size: number) =>
		readChain(start, size, miniFat, miniSectorSize, miniStream, (sector) => sector * miniSectorSize);

	function readStream(name: string): Uint8Array | null {
		const entry = entries.find((candidate) => candidate.name === name);
		if (!entry || entry.type !== 2) return null;
		return entry.size < miniCutoff
			? readMiniStream(entry.startSector, entry.size)
			: readChain(entry.startSector, entry.size);
	}

	return { entries, readStream };
}
