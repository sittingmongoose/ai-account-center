import * as fs from 'fs';

const CR = 0x0d;

/**
 * Packaged helper text with LF line endings. A package built from a Windows checkout that
 * converted line endings carries CRLF copies of the same helpers; their SHA-256 pins then failed,
 * and every remote Analytics scan stopped (2026-10-13). Pins are checked against, and hosts are
 * sent, this canonical LF text, so such an install runs exactly the reviewed program.
 *
 * Only CR LF pairs change: a lone CR is kept, so any other edit still fails its pin. The latin1
 * round trip maps every byte to itself, and no UTF-8 sequence contains a CR or LF byte.
 */
export function withLfLineEndings(bytes: Buffer): Buffer {
  if (!bytes.includes(CR)) return bytes;
  return Buffer.from(bytes.toString('latin1').replace(/\r\n/g, '\n'), 'latin1');
}

/** Reads a packaged helper as its canonical LF bytes. */
export async function readPackagedSource(file: string): Promise<Buffer> {
  return withLfLineEndings(await fs.promises.readFile(file));
}
