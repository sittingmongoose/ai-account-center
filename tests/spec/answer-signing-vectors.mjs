/**
 * The shared answer-signing case table (G16): the real TypeScript canonical JSON, Ed25519
 * signing and verification, pinned for the Rust port (crates/aac-status/src/answer_signing.rs,
 * checked by tests/answer_signing.rs). Ed25519 is deterministic: the same key and canonical
 * bytes give the same signature, so a byte-equal signature proves both directions' interop.
 *
 * The keys are fixed fixture keys (not secrets: test seeds only), so `--check` regenerates the
 * same table and compares it. It imports the TypeScript sources, so it runs in Bun:
 *
 *   bun tests/spec/answer-signing-vectors.mjs [--check]
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  canonicalJson,
  loadOrCreateAnswerKey,
  signAnswer,
  signDocument,
  verifySignedAnswer,
  verifySignedDocument,
} from '../../src/web-server/status/answer-signing.ts';

const root = fileURLToPath(new URL('../..', import.meta.url));
const out = path.join(root, 'tests/spec/answer-signing.json');
// A fixed Ed25519 key as a PKCS#8 v1 PEM file, exactly as Node writes one: the DER prefix
// (RFC 8410 section 7) and a 32-byte fixture seed.
const fixtureKeyDir = (prefix, seedByte) => {
  const der = Buffer.concat([
    Buffer.from('302e020100300506032b657004220420', 'hex'),
    Buffer.alloc(32, seedByte),
  ]);
  const body = der.toString('base64');
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  fs.writeFileSync(
    path.join(folder, 'answer-key.pem'),
    `-----BEGIN PRIVATE KEY-----
${body}
-----END PRIVATE KEY-----
`,
    { mode: 0o600 }
  );
  return folder;
};
const dir = fixtureKeyDir('aac-answer-signing-', 0x11);
const key = loadOrCreateAnswerKey(dir);
const privatePem = key.privateKey.export({ type: 'pkcs8', format: 'pem' }).toString();

const canonical = [
  null,
  true,
  false,
  0,
  -0,
  1,
  1.0,
  0.1,
  1e21,
  1e-7,
  9007199254740991,
  9007199254740993,
  -1.5,
  '',
  'plain',
  'quote " back \\ slash',
  'controls \b \f \n \r \t \u0001 \u001f end',
  'line\u2028sep\u2029par',
  '日本語 emoji 🙂',
  [],
  [1, 'two', null, [3]],
  {},
  { b: 1, a: 2 },
  { Z: 1, a: 2, A: 3, z: 4, '1': 5, '01': 6, 'ä': 7, 'a\u0308': 8 },
  { nested: { z: [1, { y: 2, x: 3 }], a: null }, top: 'ok' },
  { schema: 'agent-reports/answer@1', cardId: 'vm.lead--q1', choice: ['A'], note: '' },
];

const answer = (fields = {}) => ({
  schema: 'agent-reports/answer@1',
  cardId: 'vm.lead--q1',
  cardSeq: 3,
  cardSha256: 'a'.repeat(64),
  kind: 'answer',
  choice: ['A'],
  note: 'Go with A.\nSecond line.',
  via: 'deck',
  answeredAt: '2026-11-18T12:00:00.000Z',
  answeredBy: 'owner',
  device: 'fixture-device',
  ...fields,
});

const signed = [];
for (const fields of [{}, { kind: 'note', choice: [], note: 'A written response.' }, { kind: 'object', choice: [] }, { kind: 'done', choice: [] }, { via: 'recommended', note: '' }]) {
  const unsigned = answer(fields);
  const doc = signAnswer(unsigned, key);
  signed.push({ unsigned, signed: doc });
}

const summary = {
  schema: 'agent-reports/send@1',
  sendId: 'send-1',
  at: '2026-11-18T12:00:01.000Z',
  answeredBy: 'owner',
  device: 'fixture-device',
  entries: [
    { cardId: 'vm.lead--q1', seq: 3, cardSha256: 'a'.repeat(64), kind: 'answer', choice: ['A'], note: 'Go with A.' },
  ],
};
const signedSummary = signDocument(summary, key);

// Verification refusals, each with the fixed reason the TS checker gives.
const first = signed[0].signed;
const text = (doc) => `${JSON.stringify(doc, null, 2)}\n`;
const tampered = JSON.parse(JSON.stringify(first));
tampered.note = 'tampered';
const wrongKeyDir = fixtureKeyDir('aac-answer-signing-other-', 0x22);
const otherKey = loadOrCreateAnswerKey(wrongKeyDir);
const badBase64 = JSON.parse(JSON.stringify(first));
badBase64.signature.value = '!!!!';
const shortSig = JSON.parse(JSON.stringify(first));
shortSig.signature.value = Buffer.alloc(32).toString('base64');
const nonCanonicalBase64 = JSON.parse(JSON.stringify(first));
nonCanonicalBase64.signature.value = `${first.signature.value.slice(0, 40)} ${first.signature.value.slice(40)}`;
const unsignedDoc = JSON.parse(JSON.stringify(first));
delete unsignedDoc.signature;
const wrongSchema = JSON.parse(JSON.stringify(first));
wrongSchema.schema = 'agent-reports/send@1';

const refusals = [
  { name: 'not valid JSON', text: '{bad', schema: 'agent-reports/answer@1', reason: 'not valid JSON' },
  { name: 'array root', text: '[1]', schema: 'agent-reports/answer@1', reason: 'not an answer' },
  { name: 'wrong schema', text: text(wrongSchema), schema: 'agent-reports/answer@1', reason: 'not an answer' },
  { name: 'unsigned', text: text(unsignedDoc), schema: 'agent-reports/answer@1', reason: 'unsigned' },
  { name: 'tampered', text: text(tampered), schema: 'agent-reports/answer@1', reason: 'bad signature' },
  { name: 'bad base64', text: text(badBase64), schema: 'agent-reports/answer@1', reason: 'bad signature' },
  { name: 'short signature', text: text(shortSig), schema: 'agent-reports/answer@1', reason: 'bad signature' },
  { name: 'non-canonical base64', text: text(nonCanonicalBase64), schema: 'agent-reports/answer@1', reason: 'bad signature' },
  { name: 'another key', text: text(first), publicKeyPem: otherKey.publicKeyPem, schema: 'agent-reports/answer@1', reason: 'signed by another key' },
  { name: 'unreadable key', text: text(first), publicKeyPem: 'not a pem', schema: 'agent-reports/answer@1', reason: 'the public key is not readable' },
];
for (const refusal of refusals) {
  const held = verifySignedDocument(refusal.text, refusal.publicKeyPem ?? key.publicKeyPem, refusal.schema);
  assert.equal(held.ok, false, refusal.name);
  assert.equal(held.reason, refusal.reason, refusal.name);
}
for (const { signed: doc } of signed) {
  const held = verifySignedAnswer(text(doc), key.publicKeyPem);
  assert.equal(held.ok, true);
}
assert.equal(verifySignedDocument(text(signedSummary), key.publicKeyPem, 'agent-reports/send@1').ok, true);

const corpus = {
  version: 1,
  privatePem,
  publicKeyPem: key.publicKeyPem,
  keyId: key.keyId,
  canonical: canonical.map((value) => ({ value, text: canonicalJson(value) })),
  signed,
  summary: { unsigned: summary, signed: signedSummary },
  refusals: refusals.map(({ name, text, publicKeyPem, schema, reason }) => ({
    name,
    text,
    publicKeyPem: publicKeyPem ?? null,
    schema,
    reason,
  })),
};

if (process.argv.includes('--check')) {
  const held = JSON.parse(fs.readFileSync(out, 'utf8'));
  assert.deepEqual(JSON.parse(JSON.stringify(corpus)), held);
  console.log(`answer-signing vectors: ${canonical.length} canonical, ${signed.length} signed, ${refusals.length} refusals match`);
} else {
  fs.writeFileSync(out, `${JSON.stringify(corpus, null, 2)}\n`);
  console.log(`wrote ${out}`);
}
fs.rmSync(dir, { recursive: true, force: true });
fs.rmSync(wrongKeyDir, { recursive: true, force: true });
