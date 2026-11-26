// Encrypted-document support: a password-protected .docx is a CFB (OLE2)
// container holding `EncryptionInfo` + `EncryptedPackage`, not a zip, so it
// never reaches the renderer. This module classifies the container and, given a
// password, hands back the decrypted OOXML package for `renderAsync`.
//
// Framework-agnostic on purpose: the browser viewer, the Vue component and the
// React component all share this one implementation and only own their own
// password prompt.
import { isCompoundFile, readCompoundFile } from './cfb';
import { decryptAgilePackage, parseAgileEncryptionInfo } from './agile';
import { DocxEncryptionUnsupportedError } from './errors';

export { DocxPasswordError, DocxEncryptionUnsupportedError } from './errors';
export { parseAgileEncryptionInfo } from './agile';
export type { AgileEncryptionInfo } from './agile';

export type OfficeFileKind =
	/** OOXML package (zip) — render it directly. */
	| 'ooxml'
	/** Password-protected: needs `decryptDocx` with a password. */
	| 'encrypted'
	/** Legacy binary .doc / .wps (also a CFB container, but no OOXML inside). */
	| 'legacy-binary'
	/** Neither — let the renderer report it. */
	| 'unknown';

/** Cheap classification of the container, before any parsing/decryption. */
export function detectOfficeFileKind(data: ArrayBuffer | Uint8Array): OfficeFileKind {
	const bytes = toBytes(data);
	if (bytes.length >= 4 && bytes[0] === 0x50 && bytes[1] === 0x4b && bytes[2] === 0x03 && bytes[3] === 0x04) {
		return 'ooxml';
	}
	if (!isCompoundFile(bytes)) return 'unknown';
	const compound = readCompoundFile(bytes);
	if (!compound) return 'unknown';
	return compound.readStream('EncryptionInfo') && compound.readStream('EncryptedPackage')
		? 'encrypted'
		: 'legacy-binary';
}

/** True when the file needs a password (same check as `detectOfficeFileKind`). */
export function isEncryptedDocx(data: ArrayBuffer | Uint8Array): boolean {
	return detectOfficeFileKind(data) === 'encrypted';
}

/**
 * Decrypts a password-protected OOXML file and returns the plain .docx bytes.
 *
 * @throws DocxPasswordError when the password is wrong.
 * @throws DocxEncryptionUnsupportedError for schemes we can't handle
 *   (Office 2007 "Standard", certificate encryption, corrupt descriptors).
 */
export async function decryptDocx(
	data: ArrayBuffer | Uint8Array,
	password: string,
): Promise<ArrayBuffer> {
	const bytes = toBytes(data);
	const compound = readCompoundFile(bytes);
	if (!compound) throw new DocxEncryptionUnsupportedError('not a compound file');
	const encryptionInfo = compound.readStream('EncryptionInfo');
	const encryptedPackage = compound.readStream('EncryptedPackage');
	if (!encryptionInfo || !encryptedPackage) {
		throw new DocxEncryptionUnsupportedError('no EncryptionInfo/EncryptedPackage stream');
	}
	const agile = parseAgileEncryptionInfo(encryptionInfo);
	if (!agile) {
		throw new DocxEncryptionUnsupportedError('only ECMA-376 Agile encryption is supported');
	}
	const plain = await decryptAgilePackage(agile, password, encryptedPackage);
	return plain.buffer.slice(plain.byteOffset, plain.byteOffset + plain.byteLength) as ArrayBuffer;
}

function toBytes(data: ArrayBuffer | Uint8Array): Uint8Array {
	return data instanceof Uint8Array ? data : new Uint8Array(data);
}
