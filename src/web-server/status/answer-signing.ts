/**
 * Signed answers (agent-reports/answer@1): canonical JSON, Ed25519 signing with Node's crypto and
 * the matching check. `verifySignedAnswer` is the function a reporter mirrors: it takes the answer
 * file's text and the published public key (PEM) and says whether the file is a genuine answer.
 */
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  randomBytes,
  sign,
  verify,
} from 'crypto';
import type { KeyObject } from 'crypto';
import fs from 'fs';
import path from 'path';
import { ValidationError } from '../../errors/error-types';
import { ANSWER_SCHEMA } from './report-schema';

export interface AnswerSignature {
  alg: 'ed25519';
  keyId: string;
  value: string;
}

export interface UnsignedAnswer {
  schema: typeof ANSWER_SCHEMA;
  cardId: string;
  cardSeq: number;
  cardSha256: string;
  /**
   * answer: a choice (question card); note: a note alone, the card stays open; object: the owner
   * objects to an objection card, in their own words; done: an action card is done.
   */
  kind: 'answer' | 'note' | 'object' | 'done';
  choice: string[];
  note: string;
  /** recommended: filled by "Use the recommendations" and not changed by the owner. */
  via: 'deck' | 'recommended';
  answeredAt: string;
  answeredBy: 'owner';
  device: string;
}

export interface SignedAnswer extends UnsignedAnswer {
  signature: AnswerSignature;
}

/**
 * Canonical JSON in the RFC 8785 style: object keys sorted by UTF-16 code unit at every depth, no
 * whitespace, strings and numbers as JSON.stringify writes them (the serialization RFC 8785 uses),
 * arrays in order. Sign and verify its UTF-8 bytes.
 */
export function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') {
    const text = JSON.stringify(value);
    if (text === undefined) throw new TypeError('Canonical JSON cannot hold this value.');
    return text;
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record)
    .filter((key) => record[key] !== undefined)
    .sort();
  return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(',')}}`;
}

/** First 16 hex characters of the SHA-256 of the public key's DER (SPKI) bytes. */
export function answerKeyId(publicKey: KeyObject): string {
  const der = publicKey.export({ type: 'spki', format: 'der' });
  return createHash('sha256').update(der).digest('hex').slice(0, 16);
}

export interface AnswerKey {
  privateKey: KeyObject;
  publicKeyPem: string;
  keyId: string;
}

/**
 * `<dir>/answer-key.pem` (PKCS#8, 0600), created on first use. A second process racing the creation
 * reads the file the first one wrote.
 */
export function loadOrCreateAnswerKey(dir: string): AnswerKey {
  const file = path.join(dir, 'answer-key.pem');
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  let pem: string;
  try {
    pem = fs.readFileSync(file, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    const { privateKey } = generateKeyPairSync('ed25519');
    const created = privateKey.export({ type: 'pkcs8', format: 'pem' }).toString();
    // Written whole to a private temp file, synced, then linked into place: a crash or a second process never sees
    // half a key, and if another process got there first its key is the one used.
    const temp = path.join(dir, `.answer-key.${process.pid}.${randomBytes(6).toString('hex')}.tmp`);
    try {
      const fd = fs.openSync(temp, 'wx', 0o600);
      try {
        fs.writeFileSync(fd, created);
        fs.fsyncSync(fd);
      } finally {
        fs.closeSync(fd);
      }
      try {
        fs.linkSync(temp, file);
        pem = created;
      } catch (linkError) {
        if ((linkError as NodeJS.ErrnoException).code !== 'EEXIST') throw linkError;
        pem = fs.readFileSync(file, 'utf8');
      }
    } finally {
      fs.rmSync(temp, { force: true });
    }
  }
  try {
    fs.chmodSync(file, 0o600);
  } catch {
    /* Best effort on file systems without modes. */
  }
  const privateKey = createPrivateKey(pem);
  if (privateKey.asymmetricKeyType !== 'ed25519')
    throw new ValidationError('The answer key is not an Ed25519 key.');
  const publicKey = createPublicKey(privateKey);
  return {
    privateKey,
    publicKeyPem: publicKey.export({ type: 'spki', format: 'pem' }).toString(),
    keyId: answerKeyId(publicKey),
  };
}

export function signAnswer(answer: UnsignedAnswer, key: AnswerKey): SignedAnswer {
  const value = sign(null, Buffer.from(canonicalJson(answer), 'utf8'), key.privateKey).toString(
    'base64'
  );
  return { ...answer, signature: { alg: 'ed25519', keyId: key.keyId, value } };
}

/** Signs any AAC document the same way as an answer (used for the send summary). */
export function signDocument<T extends object>(
  doc: T,
  key: AnswerKey
): T & { signature: AnswerSignature } {
  const value = sign(null, Buffer.from(canonicalJson(doc), 'utf8'), key.privateKey).toString(
    'base64'
  );
  return { ...doc, signature: { alg: 'ed25519', keyId: key.keyId, value } };
}

export type AnswerCheck = { ok: true; answer: SignedAnswer } | { ok: false; reason: string };

/**
 * Checks one answer file against the published public key: the schema, the key id, and the
 * Ed25519 signature over the canonical JSON of the answer without `signature`. Anything else is
 * not an answer.
 */
export function verifySignedAnswer(fileText: string | Buffer, publicKeyPem: string): AnswerCheck {
  return verifySignedDocument(fileText, publicKeyPem, ANSWER_SCHEMA) as AnswerCheck;
}

/** The same check for any signed AAC document whose `schema` must equal `schema`. */
export function verifySignedDocument(
  fileText: string | Buffer,
  publicKeyPem: string,
  schema: string
):
  | { ok: true; answer: Record<string, unknown> & { signature: AnswerSignature } }
  | { ok: false; reason: string } {
  let doc: unknown;
  try {
    doc = JSON.parse(typeof fileText === 'string' ? fileText : fileText.toString('utf8'));
  } catch {
    return { ok: false, reason: 'not valid JSON' };
  }
  if (!doc || typeof doc !== 'object' || Array.isArray(doc))
    return { ok: false, reason: 'not an answer' };
  const record = doc as Record<string, unknown>;
  if (record.schema !== schema) return { ok: false, reason: 'not an answer' };
  const signature = record.signature as Record<string, unknown> | undefined;
  if (
    !signature ||
    typeof signature !== 'object' ||
    signature.alg !== 'ed25519' ||
    typeof signature.value !== 'string' ||
    typeof signature.keyId !== 'string'
  )
    return { ok: false, reason: 'unsigned' };
  let publicKey: KeyObject;
  try {
    publicKey = createPublicKey(publicKeyPem);
  } catch {
    return { ok: false, reason: 'the public key is not readable' };
  }
  if (publicKey.asymmetricKeyType !== 'ed25519')
    return { ok: false, reason: 'the public key is not an Ed25519 key' };
  if (signature.keyId !== answerKeyId(publicKey))
    return { ok: false, reason: 'signed by another key' };
  const { signature: _omit, ...unsigned } = record;
  const raw = Buffer.from(signature.value, 'base64');
  if (raw.length !== 64 || raw.toString('base64') !== signature.value)
    return { ok: false, reason: 'bad signature' };
  let good = false;
  try {
    good = verify(null, Buffer.from(canonicalJson(unsigned), 'utf8'), publicKey, raw);
  } catch {
    good = false;
  }
  return good
    ? { ok: true, answer: record as Record<string, unknown> & { signature: AnswerSignature } }
    : { ok: false, reason: 'bad signature' };
}
