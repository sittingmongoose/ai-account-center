/**
 * G16 finding 1 gate: the TypeScript key loader, run in Bun, reads the answer key file the
 * Rust signer wrote (crates/aac-status/tests/answer_signing.rs runs this). Prints the key's
 * public facts, the PKCS#8 PEM Node/Bun export for that key, and the file's bytes after the
 * load, so the caller proves TS neither moved the key aside nor made a new one.
 *
 *   bun tests/spec/answer-key-import.ts <key dir>
 */
import fs from 'node:fs';
import path from 'node:path';
import { loadOrCreateAnswerKey } from '../../src/web-server/status/answer-signing';

const dir = process.argv[2];
if (!dir) throw new Error('usage: bun tests/spec/answer-key-import.ts <key dir>');
const key = loadOrCreateAnswerKey(dir);
process.stdout.write(
  JSON.stringify({
    runtime: typeof Bun === 'undefined' ? 'node' : 'bun',
    keyId: key.keyId,
    publicKeyPem: key.publicKeyPem,
    exportedPem: key.privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
    fileAfter: fs.readFileSync(path.join(dir, 'answer-key.pem'), 'utf8'),
    entries: fs.readdirSync(dir).sort(),
  })
);
