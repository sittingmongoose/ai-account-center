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
   * answer: a choice (question card); note: a written response, closing this card; object: the owner
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

/** A key file another process is still writing (or a crash left half of) is read again for this long. */
const KEY_READ_RETRIES = 5;
const KEY_READ_PAUSE_MS = 20;
/** A key file that is still empty or unreadable this long after its last write is broken: it is replaced. */
const BROKEN_KEY_AGE_MS = 60 * 1000;

function pause(ms: number): void {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** `file`'s status, or `null` when it is gone. */
function statIfThere(file: string): fs.Stats | null {
  try {
    return fs.statSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

/** The key in `file`, `null` when the file holds no readable key, or `undefined` when there is no file. */
function readKey(file: string): KeyObject | null | undefined {
  let pem: string;
  try {
    pem = fs.readFileSync(file, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
  try {
    return createPrivateKey(pem);
  } catch {
    return null;
  }
}

/**
 * Creates the key file without ever showing another process half a key: the key is written whole to a private temp
 * file, synced, then linked into place. If another process got there first, its key is the one used. Where hard
 * links fail (some network and Windows file systems), the file is created in place, exclusively.
 */
function createKeyFile(dir: string, file: string): void {
  const { privateKey } = generateKeyPairSync('ed25519');
  const created = privateKey.export({ type: 'pkcs8', format: 'pem' }).toString();
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
    } catch (linkError) {
      if ((linkError as NodeJS.ErrnoException).code === 'EEXIST') return;
      try {
        fs.writeFileSync(file, created, { mode: 0o600, flag: 'wx' });
      } catch (writeError) {
        if ((writeError as NodeJS.ErrnoException).code !== 'EEXIST') throw writeError;
      }
    }
    // the new name is durable only once its folder is
    try {
      const folder = fs.openSync(dir, 'r');
      try {
        fs.fsyncSync(folder);
      } finally {
        fs.closeSync(folder);
      }
    } catch {
      /* not every platform syncs a folder */
    }
  } finally {
    fs.rmSync(temp, { force: true });
  }
}

/**
 * `<dir>/answer-key.pem` (PKCS#8, 0600), created on first use; a second process racing the creation uses the key
 * the first one wrote. A key file that is empty or unreadable is read again a few times (another process may be
 * writing it in place); one still broken a minute after its last write is moved aside and a new key made.
 * The answer store's writer lock (writer-lock.ts) is held before this runs, so no second process creates or repairs
 * the key at the same time.
 */
export function loadOrCreateAnswerKey(dir: string): AnswerKey {
  const file = path.join(dir, 'answer-key.pem');
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  // a crash between writing and cleaning up leaves a private key in a temp file: remove any an hour old
  try {
    for (const name of fs.readdirSync(dir)) {
      if (!/^\.answer-key\..+\.tmp$/.test(name)) continue;
      const stale = path.join(dir, name);
      if (Date.now() - fs.lstatSync(stale).mtimeMs > 60 * 60 * 1000)
        fs.rmSync(stale, { force: true });
    }
  } catch {
    /* best effort */
  }
  let privateKey: KeyObject | null | undefined;
  for (let attempt = 0; ; attempt += 1) {
    // creating and moving aside each take a turn: a file that keeps vanishing or breaking is an error, not a loop
    if (attempt > KEY_READ_RETRIES + 4)
      throw new ValidationError('The answer key file cannot be created or read.');
    privateKey = readKey(file);
    if (privateKey === undefined) {
      createKeyFile(dir, file);
      continue;
    }
    if (privateKey !== null) break;
    if (attempt < KEY_READ_RETRIES) {
      pause(KEY_READ_PAUSE_MS);
      continue;
    }
    // another process may move the broken file aside at any moment: then it is read again
    const broken = statIfThere(file);
    if (broken === null) continue;
    if (Date.now() - broken.mtimeMs < BROKEN_KEY_AGE_MS)
      throw new ValidationError('The answer key is being written. Try again in a minute.');
    // moved aside (removed with the stale temp files), only while it is still the same broken file
    if (statIfThere(file)?.ino === broken.ino) {
      try {
        fs.renameSync(
          file,
          path.join(dir, `.answer-key.broken.${process.pid}.${randomBytes(6).toString('hex')}.tmp`)
        );
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      }
    }
  }
  try {
    fs.chmodSync(file, 0o600);
  } catch {
    /* Best effort on file systems without modes. */
  }
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
