/** Shared AAC refusal policy. Status keeps compatibility exports; new callers import here. */
import packagedPolicy from './secret-policy.json';
import { ValidationError } from '../errors/error-types';

export const SECRET_POLICY_PATH = 'src/shared/secret-policy.json';
export type SecretPolicy = typeof packagedPolicy;

/** Return an independent copy so consumers cannot mutate the detector's cached rules. */
export function loadSecretPolicy(): SecretPolicy {
  return structuredClone(packagedPolicy);
}

const secretPolicy = loadSecretPolicy();

/**
 * Secret patterns: a file that matches one is refused whole (the format says no secrets). Each
 * entry names the kind it found; the matched text is never shown.
 */
// This packaged policy is also compiled into the native capture boundary. Keep refusal
// patterns, credential spellings and scan budgets in one source of truth.
const SECRET_PATTERNS: ReadonlyArray<readonly [string, (text: string) => boolean]> =
  secretPolicy.patterns.map((entry) => {
    if (entry.scan === 'jwt') return [entry.kind, hasJsonWebToken] as const;
    if (entry.scan === 'email') return [entry.kind, hasEmailAddress] as const;
    if (entry.scan === 'authorization') return [entry.kind, hasAuthorization] as const;
    if (typeof entry.pattern !== 'string')
      throw new ValidationError('Invalid report secret policy');
    const pattern = new RegExp(entry.pattern, entry.flags);
    return [entry.kind, (text: string) => pattern.test(text)] as const;
  });

/*
 * These checks run on the main thread over files any writer can fill, up to 512 KB. A regex that
 * starts with a repeat, or can start at every token of a long run, is O(n^2) on such a file (64 KB
 * of `a` took 4.7 s), so the two that can are searched from their one literal instead.
 */

const EMAIL_LOCAL_CHAR = new RegExp(secretPolicy.email.localChar);
const EMAIL_DOMAIN = new RegExp(secretPolicy.email.domain);
/** A domain is at most 253 characters (RFC 1035). */
const EMAIL_DOMAIN_WINDOW = secretPolicy.email.domainWindow;

/** `local@domain.tld`: one local character before an `@` and a domain after it. */
function hasEmailAddress(text: string): boolean {
  for (let at = text.indexOf('@'); at !== -1; at = text.indexOf('@', at + 1)) {
    if (at === 0 || !EMAIL_LOCAL_CHAR.test(text[at - 1])) continue;
    if (EMAIL_DOMAIN.test(text.slice(at + 1, at + 1 + EMAIL_DOMAIN_WINDOW))) return true;
  }
  return false;
}

const JWT_TAIL = new RegExp(
  `\\.${secretPolicy.jwt.prefix}${secretPolicy.jwt.char}{${secretPolicy.jwt.min},}\\.${secretPolicy.jwt.char}{${secretPolicy.jwt.signatureMin},}`,
  'y'
);
const JWT_CHAR = new RegExp(secretPolicy.jwt.char);
const WORD_CHAR = new RegExp(secretPolicy.jwt.wordChar);
const JWT_HEAD_MIN = secretPolicy.jwt.min;

/** `eyJ...` `.eyJ...` `.sig`, found from its `.eyJ`. Each scan stops at the next dot, so it is linear. */
function hasJsonWebToken(text: string): boolean {
  for (
    let dot = text.indexOf(`.${secretPolicy.jwt.prefix}`);
    dot !== -1;
    dot = text.indexOf(`.${secretPolicy.jwt.prefix}`, dot + 1)
  ) {
    JWT_TAIL.lastIndex = dot;
    if (!JWT_TAIL.test(text)) continue;
    let start = dot;
    while (start > 0 && JWT_CHAR.test(text[start - 1])) start -= 1;
    // The earliest `eyJ` that starts a word leaves the longest head; a later one cannot do better.
    let head = text.indexOf(secretPolicy.jwt.prefix, start);
    while (head !== -1 && head + secretPolicy.jwt.prefix.length + JWT_HEAD_MIN <= dot) {
      if (head === 0 || !WORD_CHAR.test(text[head - 1])) return true;
      head = text.indexOf(secretPolicy.jwt.prefix, head + 1);
    }
  }
  return false;
}

const AUTHORIZATION = new RegExp(secretPolicy.authorization.pattern, 'gi');
const AUTH_DIGIT = new RegExp(secretPolicy.authorization.digit);
const AUTH_UPPER = new RegExp(secretPolicy.authorization.upper);
const AUTH_LOWER = new RegExp(secretPolicy.authorization.lower);
const AUTH_BASE64 = new RegExp(secretPolicy.authorization.base64);

/** Preserve legacy credential shapes and recognize even short Basic user:password values. */
function hasAuthorization(text: string): boolean {
  AUTHORIZATION.lastIndex = 0;
  for (let match = AUTHORIZATION.exec(text); match; match = AUTHORIZATION.exec(text)) {
    const value = match[2];
    if (value.length >= secretPolicy.authorization.opaqueMin) return true;
    if (
      value.length >= secretPolicy.authorization.legacyMin &&
      (AUTH_DIGIT.test(value) || (AUTH_UPPER.test(value) && AUTH_LOWER.test(value)))
    )
      return true;
    if (
      match[1].toLowerCase() === 'basic' &&
      value.length >= secretPolicy.authorization.basicMin &&
      AUTH_BASE64.test(value)
    ) {
      const decoded = Buffer.from(value, 'base64');
      if (
        decoded.includes(0x3a) &&
        decoded.toString('base64').replace(/=+$/, '') === value.replace(/=+$/, '')
      )
        return true;
    }
  }
  return false;
}

/** The kind of secret a text contains, or null. */
export function findSecret(text: string): string | null {
  for (const [kind, found] of SECRET_PATTERNS) if (found(text)) return kind;
  return null;
}

/** A key that names a credential, whose string value is refused (decoded, so `"pass\u0077ord"` counts). */
const CREDENTIAL_KEY = new RegExp(`^${secretPolicy.credentialKey}$`, 'i');
const REFUSED_KEY = new RegExp(`^${secretPolicy.refusedKey}$`, 'i');

/**
 * The secret check for a parsed JSON file: over the text as written, then over every decoded key and string of the
 * parsed value, so an escape (`"\u0073k-..."`), a newline or tab before a token (`"key:\nsk-..."`, which stays `\n`
 * when written back out) or JSON text inside a string is seen as the reader would see it. The walk is iterative: a
 * file nested thousands of levels deep (which `JSON.parse` takes and `JSON.stringify` throws on) is walked, not a
 * crash of the scan. Any hit refuses the file.
 */
export function findSecretIn(source: string, value: unknown): string | null {
  const raw = findSecret(source);
  if (raw) return raw;
  // Each entry: a value, and whether it sits under a credential key (anything long enough under one is refused). JSON
  // text inside strings is parsed and walked too, within a size budget.
  const stack: Array<[unknown, boolean]> = [[value, false]];
  const work: EmbeddedWork = { used: 0 };
  for (let entry = stack.pop(); entry !== undefined; entry = stack.pop()) {
    const [node, credential] = entry;
    if (typeof node === 'string') {
      if (credential && node.length >= secretPolicy.credentialMin) return 'a credential field';
      const found =
        findSecret(node) ?? (node.includes('\\') ? findSecret(unescapeText(node)) : null);
      if (found) return found;
      const inner = embeddedJson(node, work);
      if (inner === UNCHECKED) return 'JSON text that could not be checked';
      if (inner === UNPARSEABLE) return 'unparseable embedded JSON';
      if (inner !== undefined) stack.push([inner, credential]);
    } else if (typeof node === 'number') {
      if (credential && String(Math.abs(Math.trunc(node))).length >= secretPolicy.credentialMin)
        return 'a credential field';
    } else if (Array.isArray(node)) {
      for (const item of node) stack.push([item, credential]);
    } else if (node && typeof node === 'object') {
      for (const [key, item] of Object.entries(node as Record<string, unknown>)) {
        if (REFUSED_KEY.test(key)) return 'a credential field';
        const found = findSecret(key);
        if (found) return found;
        stack.push([item, credential || CREDENTIAL_KEY.test(key)]);
      }
    }
  }
  return null;
}

/** Maximum characters of embedded JSON parsed per file; any parse failure refuses it. */
const EMBEDDED_BUDGET = secretPolicy.embeddedBudget;
/** JSON text that can't be checked within the budget: the file is refused, not read unchecked. */
const UNCHECKED = Symbol('unchecked');
const UNPARSEABLE = Symbol('unparseable');
/** Running totals for one file's embedded JSON. */
type EmbeddedWork = { used: number };

/** JSON-looking objects/arrays need not be closed to be checked. Every parse failure
 * refuses the document; no normalization or unchecked fallback is attempted. */
function embeddedJson(text: string, work: EmbeddedWork): unknown {
  const trimmed = text.trim();
  const open = trimmed[0];
  const close = trimmed[trimmed.length - 1];
  const shaped =
    open === '{' || open === '[' || (trimmed.length >= 2 && open === '"' && close === '"');
  if (!shaped) return undefined;
  if (work.used + text.length > EMBEDDED_BUDGET) return UNCHECKED;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    work.used += text.length;
    return parsed;
  } catch {
    return UNPARSEABLE;
  }
}

/**
 * JSON text inside a string that doesn't parse, read without parsing: escapes of any depth of backslashes (`\\n`,
 * `\\\\n`, `\\"`, `\\u0073`, and up to two `\\u005c` that make another escape) become what they stand for, so a token after an escaped newline or
 * a credential key inside it is seen. Linear: one scan per pass and at most three passes (a regex starting with a
 * repeat was quadratic on a long run of backslashes).
 */
function unescapeText(text: string): string {
  let out = text;
  for (let pass = 0; pass < 3 && out.includes('\\'); pass += 1) {
    const next = unescapeOnce(out);
    if (next === out) break;
    out = next;
  }
  return out;
}

function unescapeOnce(text: string): string {
  const parts: string[] = [];
  let i = 0;
  while (i < text.length) {
    const slash = text.indexOf('\\', i);
    if (slash === -1) {
      parts.push(text.slice(i));
      break;
    }
    parts.push(text.slice(i, slash));
    let j = slash;
    while (j < text.length && text[j] === '\\') j += 1;
    const next = text[j];
    const hex = text.slice(j + 1, j + 5);
    if (next !== undefined && 'nrtbf'.includes(next)) {
      parts.push(' ');
      i = j + 1;
    } else if (next === '"' || next === '/') {
      parts.push(next);
      i = j + 1;
    } else if (next === 'u' && /^[0-9a-fA-F]{4}$/.test(hex)) {
      parts.push(String.fromCharCode(parseInt(hex, 16)));
      i = j + 5;
    } else {
      parts.push(text.slice(slash, j));
      i = j;
    }
  }
  return parts.join('');
}
