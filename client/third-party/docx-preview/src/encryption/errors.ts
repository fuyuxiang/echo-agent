/** Wrong password (the Agile verifier didn't match). */
export class DocxPasswordError extends Error {
	constructor(message = 'wrong password') {
		super(message);
		this.name = 'DocxPasswordError';
	}
}

/**
 * The file is encrypted, but not with a scheme we can decrypt: Office 2007
 * "Standard" (RC4 / AES-128-ECB), certificate (IRM) encryption, or a broken
 * descriptor.
 */
export class DocxEncryptionUnsupportedError extends Error {
	constructor(message = 'unsupported encryption') {
		super(message);
		this.name = 'DocxEncryptionUnsupportedError';
	}
}
