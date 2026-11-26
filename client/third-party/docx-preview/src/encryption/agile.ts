// ECMA-376 "Agile Encryption" (Word 2010+ / WPS default when a document is
// password protected). The scheme is fully specified and self-contained:
//
//   h            = H(passwordSalt + password_utf16le)   iterated spinCount times
//                  with h = H(u32le(i) + h)             (iterator first)
//   key(n)       = H(h + blockKey_n)[0..keyBits/8]
//   secretKey    = AES-CBC-decrypt(key(encryptedKeyValue), iv = passwordSalt)
//   verifier     = AES-CBC-decrypt(key(verifierHashInput), iv = passwordSalt)
//   verifierHash = AES-CBC-decrypt(key(verifierHashValue), iv = passwordSalt)
//   payload      = for each 4096-byte segment i:
//                    iv = H(keyDataSalt + u32le(i))[0..blockSize]
//                    plain += AES-CBC-decrypt(secretKey, iv, segment)
//
// Only the two salts differ in role: the password/verifier material hangs off
// the <p:encryptedKey> element's saltValue, the payload IVs off <keyData>'s.
import { sha1 } from '@noble/hashes/legacy.js';
import { sha256, sha384, sha512 } from '@noble/hashes/sha2.js';
import { cbc } from '@noble/ciphers/aes.js';
import { DocxEncryptionUnsupportedError, DocxPasswordError } from './errors';

const BLOCK_KEY_VERIFIER_INPUT = new Uint8Array([0xfe, 0xa7, 0xd2, 0x76, 0x3b, 0x4b, 0x9e, 0x79]);
const BLOCK_KEY_VERIFIER_VALUE = new Uint8Array([0xd7, 0xaa, 0x0f, 0x6d, 0x30, 0x61, 0x34, 0x4e]);
const BLOCK_KEY_ENCRYPTED_KEY = new Uint8Array([0x14, 0x6e, 0x0b, 0xe7, 0xab, 0xac, 0xd0, 0xd6]);
const SEGMENT_LENGTH = 4096;
// Yield to the event loop every N hash rounds so the password dialog can paint
// a spinner: spinCount is 100000 by default and a tight sync loop would freeze
// the tab for about a second.
const YIELD_EVERY = 8192;

type HashName = 'SHA1' | 'SHA256' | 'SHA384' | 'SHA512';
const HASHES: Record<HashName, (data: Uint8Array) => Uint8Array> = {
	SHA1: sha1,
	SHA256: sha256,
	SHA384: sha384,
	SHA512: sha512,
};

export interface AgileEncryptionInfo {
	spinCount: number;
	hashAlgorithm: HashName;
	keyBits: number;
	blockSize: number;
	hashSize: number;
	/** Salt of <p:encryptedKey> — password hash + verifier material. */
	passwordSalt: Uint8Array;
	/** Salt of <keyData> — payload segment IVs. */
	keyDataSalt: Uint8Array;
	encryptedKeyValue: Uint8Array;
	encryptedVerifierHashInput: Uint8Array;
	encryptedVerifierHashValue: Uint8Array;
}

/** `EncryptionInfo` stream → Agile parameters, or null when it isn't Agile/AES-CBC. */
export function parseAgileEncryptionInfo(info: Uint8Array): AgileEncryptionInfo | null {
	if (info.length < 12) return null;
	const view = new DataView(info.buffer, info.byteOffset, info.byteLength);
	const versionMajor = view.getUint16(0, true);
	const versionMinor = view.getUint16(2, true);
	const flags = view.getUint32(4, true);
	// 4.4 + the agile flag is what Word/WPS write; anything else is the legacy
	// binary descriptor or "extensible" (certificate) encryption.
	if (versionMajor !== 4 || versionMinor !== 4 || !(flags & 0x40)) return null;

	const xml = decodeXml(info.subarray(8));
	const keyData = attribute(xml, 'keyData', 'saltValue');
	const encryptedKey = element(xml, 'encryptedKey');
	if (!encryptedKey) return null;

	const cipherAlgorithm = attr(encryptedKey, 'cipherAlgorithm');
	const cipherChaining = attr(encryptedKey, 'cipherChaining');
	const hashAlgorithm = (attr(encryptedKey, 'hashAlgorithm') || 'SHA1').toUpperCase() as HashName;
	const keyBits = Number(attr(encryptedKey, 'keyBits'));
	const blockSize = Number(attr(encryptedKey, 'blockSize'));
	const hashSize = Number(attr(encryptedKey, 'hashSize'));
	const spinCount = Number(attr(encryptedKey, 'spinCount'));
	const passwordSalt = base64(attr(encryptedKey, 'saltValue'));
	const encryptedKeyValue = base64(attr(encryptedKey, 'encryptedKeyValue'));
	const encryptedVerifierHashInput = base64(attr(encryptedKey, 'encryptedVerifierHashInput'));
	const encryptedVerifierHashValue = base64(attr(encryptedKey, 'encryptedVerifierHashValue'));
	const keyDataSalt = base64(keyData);

	if (
		cipherAlgorithm !== 'AES' ||
		cipherChaining !== 'ChainingModeCBC' ||
		!(hashAlgorithm in HASHES) ||
		!passwordSalt ||
		!keyDataSalt ||
		!encryptedKeyValue ||
		!encryptedVerifierHashInput ||
		!encryptedVerifierHashValue ||
		!Number.isFinite(keyBits) ||
		!Number.isFinite(spinCount) ||
		!Number.isFinite(blockSize) ||
		!Number.isFinite(hashSize) ||
		blockSize !== 16 ||
		keyBits % 8 !== 0
	) {
		throw new DocxEncryptionUnsupportedError(
			`unsupported Agile encryption parameters (${cipherAlgorithm}/${cipherChaining}/${hashAlgorithm})`,
		);
	}

	return {
		spinCount,
		hashAlgorithm,
		keyBits,
		blockSize,
		hashSize,
		passwordSalt,
		keyDataSalt,
		encryptedKeyValue,
		encryptedVerifierHashInput,
		encryptedVerifierHashValue,
	};
}

/** Decrypts an `EncryptedPackage` stream. Throws DocxPasswordError on a bad password. */
export async function decryptAgilePackage(
	info: AgileEncryptionInfo,
	password: string,
	encryptedPackage: Uint8Array,
): Promise<Uint8Array> {
	const hash = HASHES[info.hashAlgorithm];
	const keyLength = info.keyBits >> 3;
	const iterate = await iteratedHash(hash, info.passwordSalt, password, info.spinCount);

	const keyFor = (blockKey: Uint8Array) => hash(concat(iterate, blockKey)).subarray(0, keyLength);

	const secretKey = aesCbcDecrypt(
		keyFor(BLOCK_KEY_ENCRYPTED_KEY),
		info.passwordSalt,
		info.encryptedKeyValue,
	);

	const verifierInput = aesCbcDecrypt(
		keyFor(BLOCK_KEY_VERIFIER_INPUT),
		info.passwordSalt,
		info.encryptedVerifierHashInput,
	);
	const verifierHash = aesCbcDecrypt(
		keyFor(BLOCK_KEY_VERIFIER_VALUE),
		info.passwordSalt,
		info.encryptedVerifierHashValue,
	);
	const expected = hash(verifierInput);
	if (!equals(expected, verifierHash.subarray(0, info.hashSize))) {
		throw new DocxPasswordError('the password did not match');
	}

	// ── Payload: 4096-byte segments, one derived IV each ──
	if (encryptedPackage.length < 8) throw new DocxEncryptionUnsupportedError('empty EncryptedPackage');
	const totalSize = Number(new DataView(encryptedPackage.buffer, encryptedPackage.byteOffset, 8).getBigUint64(0, true));
	const out = new Uint8Array(totalSize);
	let written = 0;
	for (let offset = 8, index = 0; written < totalSize; offset += SEGMENT_LENGTH, index++) {
		const segment = encryptedPackage.subarray(offset, Math.min(offset + SEGMENT_LENGTH, encryptedPackage.length));
		if (!segment.length) break;
		const iv = hash(concat(info.keyDataSalt, uint32(index))).subarray(0, info.blockSize);
		const plain = aesCbcDecrypt(secretKey, iv, segment);
		const take = Math.min(plain.length, totalSize - written);
		out.set(plain.subarray(0, take), written);
		written += take;
	}
	if (written < totalSize) throw new DocxEncryptionUnsupportedError('truncated EncryptedPackage');
	return out;
}

/** h = H(salt + password_utf16le), then h = H(u32le(i) + h) for i in [0, spinCount). */
async function iteratedHash(
	hash: (data: Uint8Array) => Uint8Array,
	salt: Uint8Array,
	password: string,
	spinCount: number,
): Promise<Uint8Array> {
	let h = hash(concat(salt, utf16le(password)));
	for (let i = 0; i < spinCount; i++) {
		h = hash(concat(uint32(i), h));
		if (i % YIELD_EVERY === YIELD_EVERY - 1) await yieldToHost();
	}
	return h;
}

function yieldToHost(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

function aesCbcDecrypt(key: Uint8Array, iv: Uint8Array, data: Uint8Array): Uint8Array {
	if (data.length % 16 !== 0) {
		throw new DocxEncryptionUnsupportedError('ciphertext is not block aligned');
	}
	// Office's ciphertext carries no PKCS#7 padding: the verifier/key material is
	// exactly keyBits long and the package length comes from its own header.
	return cbc(key, iv, { disablePadding: true }).decrypt(data);
}

function concat(...parts: Uint8Array[]): Uint8Array {
	let length = 0;
	for (const part of parts) length += part.length;
	const out = new Uint8Array(length);
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

function uint32(value: number): Uint8Array {
	const out = new Uint8Array(4);
	new DataView(out.buffer).setUint32(0, value, true);
	return out;
}

function utf16le(text: string): Uint8Array {
	const out = new Uint8Array(text.length * 2);
	for (let i = 0; i < text.length; i++) {
		const code = text.charCodeAt(i);
		out[i * 2] = code & 0xff;
		out[i * 2 + 1] = code >> 8;
	}
	return out;
}

function equals(a: Uint8Array, b: Uint8Array): boolean {
	if (a.length !== b.length) return false;
	let diff = 0;
	for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
	return diff === 0;
}

function base64(value: string | undefined): Uint8Array | null {
	if (!value) return null;
	try {
		const binary = atob(value);
		const out = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
		return out;
	} catch {
		return null;
	}
}

function decodeXml(bytes: Uint8Array): string {
	let text = '';
	// Strip a UTF-8 BOM when Word writes one.
	const start = bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf ? 3 : 0;
	for (let i = start; i < bytes.length; i++) text += String.fromCharCode(bytes[i]);
	// The descriptor is ASCII apart from attribute payloads we decode separately,
	// so a byte-per-char read is enough here.
	return text;
}

/** First `<tag ...>` element in the descriptor, as its attribute source text. */
function element(xml: string, tag: string): string | null {
	// Word writes the password encryptor as `<p:encryptedKey …>` — the namespace
	// prefix is optional in the descriptor, so accept either spelling.
	const match = xml.match(new RegExp(`<(?:[\\w.-]+:)?${tag}\\b[^>]*>`));
	return match ? match[0] : null;
}

function attr(source: string | null, name: string): string | undefined {
	if (!source) return undefined;
	const match = source.match(new RegExp(`${name}="([^"]*)"`));
	return match ? match[1] : undefined;
}

/** Attribute `name` of the first `<tag ...>` element. */
function attribute(xml: string, tag: string, name: string): string | undefined {
	return attr(element(xml, tag), name);
}
