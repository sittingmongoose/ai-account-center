/**
 * Shared validators of the agent report documents (report-schema.ts, card-schema.ts). Each
 * projection throws `Invalid` with a fixed reason; nothing here echoes a file's text.
 */

export const THREAD_SCHEMA = 'agent-reports/thread@1';
export const RUN_SCHEMA = 'agent-reports/run@1';
export const TASKS_SCHEMA = 'agent-reports/tasks@1';
export const CARD_SCHEMA = 'agent-reports/card@1';
export const ANSWER_SCHEMA = 'agent-reports/answer@1';

export const MACHINES = ['vm', 'mac', 'windows', 'nas1'] as const;
export type MachineId = (typeof MACHINES)[number];

/** `<machine>.<pm-mail name>`, lowercase, 64 characters at most. */
const WRITER = /^(vm|mac|windows|nas1)\.[a-z0-9][a-z0-9.-]*$/;
const SLUG = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const LOCAL_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
/** pm-mail's own name rule (`NAME_OK` in pm-mail): lowercase, no dots. */
const PM_MAIL_NAME = /^[a-z0-9][a-z0-9_-]{0,63}$/;
export const OPTION_KEY = /^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$/;
/** Control characters other than tab and newline, and bidirectional overrides. */
const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]/;
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d{1,9})?)?(Z|[+-]\d{2}:\d{2})$/;

export function isWriterName(value: unknown): value is string {
  // A card id is `<writer>--<local id>`, split on the first `--`, so a writer name cannot hold one.
  return (
    typeof value === 'string' && value.length <= 64 && WRITER.test(value) && !value.includes('--')
  );
}

export function writerMachine(writer: string): MachineId {
  return writer.slice(0, writer.indexOf('.')) as MachineId;
}

/** A name one card may ring. `all` is pm-mail's broadcast, which a card file must never reach. */
export function isPmMailName(value: unknown): value is string {
  return typeof value === 'string' && value !== 'all' && PM_MAIL_NAME.test(value);
}

/** A card id is `<writer>--<local id>`. */
export function isCardId(value: unknown): value is string {
  if (typeof value !== 'string' || value.length > 130) return false;
  const at = value.indexOf('--');
  return at > 0 && isWriterName(value.slice(0, at)) && LOCAL_ID.test(value.slice(at + 2));
}

/**
 * Secret patterns: a file that matches one is refused whole (the format says no secrets). Each
 * entry names the kind it found; the matched text is never shown.
 */
const SECRET_PATTERNS: ReadonlyArray<readonly [string, (text: string) => boolean]> = [
  ['a private key', matches(/-----BEGIN [A-Z ]*PRIVATE KEY-----/)],
  ['an API key', matches(/\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}/)],
  ['a GitHub token', matches(/\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})/)],
  ['a Slack token', matches(/\bxox[abposr]-[A-Za-z0-9-]{10,}/)],
  ['an AWS key', matches(/\bAKIA[0-9A-Z]{16}\b/)],
  ['a GitLab token', matches(/\bglpat-[A-Za-z0-9_-]{20,}/)],
  ['an npm token', matches(/\bnpm_[A-Za-z0-9]{36}\b/)],
  ['a Hugging Face token', matches(/\bhf_[A-Za-z0-9]{30,}\b/)],
  ['a Google API key', matches(/\bAIza[0-9A-Za-z_-]{35}/)],
  ['a JSON web token', hasJsonWebToken],
  ['a bearer token', matches(/\bBearer\s+[A-Za-z0-9._~+/-]{20,}/i)],
  [
    'a credential field',
    matches(
      /"(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)"\s*:\s*"[^"]{6,}"/i
    ),
  ],
  [
    'a credential in a URL',
    matches(
      /[?&](?:access_token|token|api_key|apikey|key|sig|signature|password|code)=[^&\s"]{8,}/i
    ),
  ],
  ['an email address', hasEmailAddress],
];

function matches(pattern: RegExp): (text: string) => boolean {
  return (text) => pattern.test(text);
}

/*
 * These checks run on the main thread over files any writer can fill, up to 512 KB. A regex that
 * starts with a repeat, or can start at every token of a long run, is O(n^2) on such a file (64 KB
 * of `a` took 4.7 s), so the two that can are searched from their one literal instead.
 */

const EMAIL_LOCAL_CHAR = /[A-Za-z0-9._%+-]/;
const EMAIL_DOMAIN = /^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/;
/** A domain is at most 253 characters (RFC 1035). */
const EMAIL_DOMAIN_WINDOW = 256;

/** `local@domain.tld`: one local character before an `@` and a domain after it. */
function hasEmailAddress(text: string): boolean {
  for (let at = text.indexOf('@'); at !== -1; at = text.indexOf('@', at + 1)) {
    if (at === 0 || !EMAIL_LOCAL_CHAR.test(text[at - 1])) continue;
    if (EMAIL_DOMAIN.test(text.slice(at + 1, at + 1 + EMAIL_DOMAIN_WINDOW))) return true;
  }
  return false;
}

const JWT_TAIL = /\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/y;
const JWT_CHAR = /[A-Za-z0-9_-]/;
const WORD_CHAR = /[A-Za-z0-9_]/;
const JWT_HEAD_MIN = 8;

/** `eyJ...` `.eyJ...` `.sig`, found from its `.eyJ`. Each scan stops at the next dot, so it is linear. */
function hasJsonWebToken(text: string): boolean {
  for (let dot = text.indexOf('.eyJ'); dot !== -1; dot = text.indexOf('.eyJ', dot + 1)) {
    JWT_TAIL.lastIndex = dot;
    if (!JWT_TAIL.test(text)) continue;
    let start = dot;
    while (start > 0 && JWT_CHAR.test(text[start - 1])) start -= 1;
    // The earliest `eyJ` that starts a word leaves the longest head; a later one cannot do better.
    let head = text.indexOf('eyJ', start);
    while (head !== -1 && head + 3 + JWT_HEAD_MIN <= dot) {
      if (head === 0 || !WORD_CHAR.test(text[head - 1])) return true;
      head = text.indexOf('eyJ', head + 1);
    }
  }
  return false;
}

/** The kind of secret a text contains, or null. */
export function findSecret(text: string): string | null {
  for (const [kind, found] of SECRET_PATTERNS) if (found(text)) return kind;
  return null;
}

export type Parsed<T> = { ok: true; value: T } | { ok: false; reason: string };

/**
 * Bytes of one share file -> its JSON value, under the rules every reader applies: the size cap, no
 * NUL bytes (a damaged write), UTF-8, no secrets. Every refusal is a fixed reason.
 */
export function jsonFromBytes(bytes: Buffer, capBytes: number): Parsed<unknown> {
  if (bytes.length > capBytes)
    return { ok: false, reason: `larger than the ${capBytes / 1024} KB cap` };
  if (bytes.length === 0) return { ok: false, reason: 'empty' };
  if (bytes.includes(0)) return { ok: false, reason: 'contains NUL bytes from a damaged write' };
  let source: string;
  try {
    source = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return { ok: false, reason: 'not UTF-8 text' };
  }
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    return { ok: false, reason: 'not valid JSON' };
  }
  let secret: string | null;
  try {
    secret = findSecretIn(source, value);
  } catch {
    // never let one file's shape stop a scan: it is refused and named instead
    return { ok: false, reason: 'could not be checked for secrets' };
  }
  if (secret) return { ok: false, reason: `refused: it contains what looks like ${secret}` };
  return { ok: true, value };
}

/** A key that names a credential, whose string value is refused (decoded, so `"pass\u0077ord"` counts). */
const CREDENTIAL_KEY =
  /^(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)$/i;

/**
 * A secret written the way a person types one in prose, a shell or code (the patterns above are shaped for files),
 * for the owner's notes, which go to the share. Found by its name, then judged by its value.
 * - Names: an identifier (`DB_PASSWORD`, `PGPASSWORD`, `API_KEY_2`, `clientSecret`, `x-api-key`), a short phrase
 *   (`the api key`, `db pass`), a flag (`--password`), `setx NAME`, before `=`, `:`, `=>`, `->`, `:=`, `"]=`, a
 *   quoted name's `,` (`setenv("PASSWORD", ...)`) or, after a phrase, "is"; then `Bearer`/`Basic`, a URL's password
 *   (`scheme://user:pw@`, the user may be empty), `-p<glued>` and `-u user:pw`. A name ending in a word like file,
 *   path, count, hash or manager names something else; `ssh key` and `GPG key` in prose are public ids.
 * - A bare `pass`, `pw` or `pwd` (or one in a phrase, `db pass`) is weaker evidence: never `PASS:` (a test result),
 *   and its value must hold a run of 6 or more mixing letters with digits or symbols, with no slash.
 * - Values: a reference or placeholder (`$VAR`, `%VAR%`, `<your-key>`), a URL, a date, a version (`v2.3.1`) or a
 *   `SHA256:` fingerprint is no secret; after "is", neither is `p95=320ms`. A path (from `~`, `.`, a drive or a
 *   system root, or `a/b.ext`) is split at slashes too and judged by its runs. After a password's name, any other
 *   value of 8 or more with a letter and a digit, or 8 or more digits, is one (`Summer_2026`, `Winter-2025!`);
 *   otherwise a run of 6 mixing letters with digits or symbols (`hunter2`). After a key's name, a run of 8.
 *   Runs are split at `-` `.` `_` `:`, so model names, versions and test names have none (`claude-opus-5-5`).
 * Each pattern is bounded and matches only the name, reading the value by lookahead, so a value is searched for
 * names in its turn, and the search stays linear.
 */
type SecretKind = 'password' | 'bare' | 'key';
const NAMED_VALUE =
  /(?<![A-Za-z0-9_-])((?:[A-Za-z][A-Za-z0-9]{0,23} )?\$?[A-Za-z_][A-Za-z0-9_-]{0,63})(['"]?\]?\s{0,4}(?:=>|->|:=|[:=])\s{0,4}|['"]\]?\s{0,4},\s{0,4}|\s{1,4}(?:is|was)\s{1,4})(?=['"`]?([^\s'"`]{1,256}))/g;
const SETX_VALUE =
  /(?<![A-Za-z0-9])setx\s{1,4}([A-Za-z_][A-Za-z0-9_]{0,63})\s{1,4}(?=['"]?([^\s'"]{1,256}))/gi;
const FLAG_VALUE =
  /(?<![A-Za-z0-9-])--([A-Za-z][A-Za-z0-9-]{0,47})(?:=|\s{1,4})(?=['"`]?([^\s'"`]{1,256}))/g;
const AUTH_VALUE = /(?<![A-Za-z0-9])(?:bearer|basic)\s{1,4}(?=([^\s'"`]{1,256}))/gi;
const URL_PASSWORD =
  /(?<![A-Za-z0-9+.-])[A-Za-z][A-Za-z0-9+.-]{0,31}:\/\/[^\s/:@]{0,64}:(?=([^\s/@]{1,256})@)/g;
const GLUED_PASSWORD = /(?<![A-Za-z0-9-])-p(?=([^\s'"=-][^\s'"]{0,255}))/g;
const USER_PASSWORD =
  /(?<![A-Za-z0-9-])(?:-u|--user)(?:\s{1,4}|=)['"]?[^\s:'"]{1,64}:(?=([^\s'"@]{1,256}))/g;
const NOT_A_SECRET_NAME = new Set(
  (
    'file files path dir url uri name names count limit length len size ttl type kind mode policy budget usage id ' +
    'ids at expires expiry env hash hashes manager scanning scanner fingerprint min max'
  ).split(' ')
);
const KEY_PAIR = new Set('api access private secret signing encryption master deploy'.split(' '));
const PATH_LIKE = new RegExp(
  '^(?:~[\\\\/]|\\.{1,2}[\\\\/]|[A-Za-z]:[\\\\/]|' +
    '[\\\\/](?:home|Users|etc|var|tmp|usr|opt|mnt|run|srv|root|Volumes|private|proc|dev|sys|bin|sbin|lib|lib64|' +
    'snap|media|nix|workspaces?|app|data|cygdrive)(?:[\\\\/]|$)|' +
    '[\\w.-]+(?:[\\\\/][\\w.-]+)+\\.[A-Za-z][A-Za-z0-9]{0,5}$)'
);

function secretKind(name: string, separator: string, flag: boolean): SecretKind | null {
  const phrase = name.includes(' ');
  const words = name
    .replace(/^\$/, '')
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .toLowerCase()
    .split(/[\s_-]+/)
    .filter(Boolean);
  if (words.length === 0 || NOT_A_SECRET_NAME.has(words[words.length - 1])) return null;
  if (words.some((word) => /(?:password|passwd|passphrase)$/.test(word))) return 'password';
  if (words.some((word) => word === 'pass' || word === 'pw' || word === 'pwd')) {
    if (flag || (!phrase && words.length > 1)) return 'password';
    // `PASS: TestLogin2FA` is a test result
    if (!(name === 'PASS' && separator === ':')) return 'bare';
  }
  const glued = phrase
    ? /^(?:secret|token|credentials?)$/
    : /(?:secret|token|credentials?|apikey|accesskey|privatekey|signingkey|secretkey)$/;
  if (words.some((word) => glued.test(word))) return 'key';
  for (let at = 1; at < words.length; at += 1)
    if (words[at] === 'key' && (!phrase || KEY_PAIR.has(words[at - 1]))) return 'key';
  return null;
}

/** The longest run, between `separators`, that mixes letters with digits or symbols. */
function longestMixedRun(value: string, separators: RegExp): number {
  let longest = 0;
  for (const run of value.split(separators))
    if (/[A-Za-z]/.test(run) && /[0-9!@#$%^&*+=?~|]/.test(run))
      longest = Math.max(longest, run.length);
  return longest;
}

/** Whether a value after a secret's name looks like one (see above). */
function secretValue(kind: SecretKind, raw: string, afterIs = false): boolean {
  if (/^(?:\$\{?[A-Za-z_][A-Za-z0-9_]*\}?|%[A-Za-z_][A-Za-z0-9_]*%|<[^>]*>?|\{\{.*)$/.test(raw))
    return false;
  const value = raw.replace(/^[([]+/, '').replace(/[)\]}>.,;:!?]+$/, '');
  if (
    /^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(value) ||
    /^\d{4}-\d{2}-\d{2}/.test(value) ||
    /^v?\d+(?:\.\d+)+(?:[-+][\w.]+)?$/.test(value) ||
    /^(?:SHA256|SHA1|MD5):/i.test(value) ||
    (afterIs && value.includes('='))
  )
    return false;
  if (PATH_LIKE.test(value))
    return kind !== 'bare' && longestMixedRun(value, /[-._:\\/]+/) >= (kind === 'key' ? 8 : 6);
  if (kind === 'key') return longestMixedRun(value, /[-._:]+/) >= 8;
  if (kind === 'bare') return !/[\\/]/.test(value) && longestMixedRun(value, /[-._:]+/) >= 6;
  // a password: human ones are a word and a number (`Summer_2026`), so the whole value counts
  if (/^\d{8,}$/.test(value)) return true;
  if (value.length >= 8 && /[A-Za-z]/.test(value) && /\d/.test(value)) return true;
  return longestMixedRun(value, /[-._:]+/) >= 6;
}

export function findTypedSecret(text: string): string | null {
  const found = (kind: SecretKind | null, value: string | undefined, afterIs = false) =>
    kind !== null && value !== undefined && secretValue(kind, value, afterIs);
  for (const match of text.matchAll(NAMED_VALUE)) {
    const separator = match[2].replace(/['"\]\s]/g, '');
    const afterIs = separator === 'is' || separator === 'was';
    if (found(secretKind(match[1], separator, false), match[3], afterIs))
      return 'a password or key';
  }
  for (const match of text.matchAll(SETX_VALUE))
    if (found(secretKind(match[1], '=', false), match[2])) return 'a password or key';
  for (const match of text.matchAll(FLAG_VALUE))
    if (found(secretKind(match[1], '=', true), match[2])) return 'a password or key';
  for (const pattern of [AUTH_VALUE, URL_PASSWORD, GLUED_PASSWORD, USER_PASSWORD])
    for (const match of text.matchAll(pattern))
      if (found('password', match[1])) return 'a password or key';
  return null;
}

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
  const work: EmbeddedWork = { used: 0, failed: 0 };
  for (let entry = stack.pop(); entry !== undefined; entry = stack.pop()) {
    const [node, credential] = entry;
    if (typeof node === 'string') {
      if (credential && node.length >= 6) return 'a credential field';
      const found =
        findSecret(node) ?? (node.includes('\\') ? findSecret(unescapeText(node)) : null);
      if (found) return found;
      const inner = embeddedJson(node, work);
      if (inner === UNCHECKED) return 'JSON text that could not be checked';
      if (inner !== undefined) stack.push([inner, credential]);
    } else if (typeof node === 'number') {
      if (credential && String(Math.abs(Math.trunc(node))).length >= 6) return 'a credential field';
    } else if (Array.isArray(node)) {
      for (const item of node) stack.push([item, credential]);
    } else if (node && typeof node === 'object') {
      for (const [key, item] of Object.entries(node as Record<string, unknown>)) {
        const found = findSecret(key);
        if (found) return found;
        stack.push([item, credential || CREDENTIAL_KEY.test(key)]);
      }
    }
  }
  return null;
}

/** How many characters of JSON text inside strings are parsed per file, and how many such parses may fail. */
const EMBEDDED_BUDGET = 4 * 1024 * 1024;
const EMBEDDED_FAILURES = 256;
/** JSON text that can't be checked within the budget: the file is refused, not read unchecked. */
const UNCHECKED = Symbol('unchecked');
/** A credential key written anywhere in text (quoted, as JSON writes keys). */
const CREDENTIAL_KEY_TEXT =
  /"(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)"/i;

/** Running totals for one file's embedded JSON. */
type EmbeddedWork = { used: number; failed: number };

/**
 * A string that is itself JSON (an object, an array or a string, as double-encoded text is), parsed; `UNCHECKED` when it
 * can't be checked within the budget; undefined for text that isn't JSON. Only text whose last character closes its first is parsed, and
 * after EMBEDDED_FAILURES failed parses no more are tried (each failure throws, which is slow in bulk).
 */
function embeddedJson(text: string, work: EmbeddedWork): unknown {
  const trimmed = text.trim();
  const open = trimmed[0];
  const close = trimmed[trimmed.length - 1];
  const shaped =
    trimmed.length >= 2 &&
    ((open === '{' && close === '}') ||
      (open === '[' && close === ']') ||
      (open === '"' && close === '"'));
  if (!shaped) return undefined;
  // past the failure cap or the size budget, text that names a credential key can't be read unchecked
  if (work.failed >= EMBEDDED_FAILURES || work.used + text.length > EMBEDDED_BUDGET)
    return CREDENTIAL_KEY_TEXT.test(unescapeText(text)) || work.used + text.length > EMBEDDED_BUDGET
      ? UNCHECKED
      : undefined;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    work.used += text.length;
    return parsed;
  } catch {
    work.failed += 1;
    return undefined;
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

/** Thrown inside the projection; becomes `{ ok: false, reason }`. */
export class Invalid extends Error {}

export function fail(field: string): never {
  throw new Invalid(`${field} is missing or not valid`);
}

export function obj(value: unknown, field: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail(field);
  return value as Record<string, unknown>;
}

export function text(value: unknown, field: string, max: number): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > max ||
    CONTROL.test(value) ||
    LONE_SURROGATE.test(value)
  )
    fail(field);
  return value;
}

export function optText(value: unknown, field: string, max: number): string | null {
  return value === null || value === undefined || value === '' ? null : text(value, field, max);
}

export function slug(value: unknown, field: string): string {
  if (typeof value !== 'string' || !SLUG.test(value)) fail(field);
  return value;
}

export function optObj<T>(
  value: unknown,
  field: string,
  read: (value: Record<string, unknown>) => T
): T | null {
  return value === null || value === undefined ? null : read(obj(value, field));
}

export function optSlug(value: unknown, field: string): string | null {
  return value === null || value === undefined ? null : slug(value, field);
}

export function count(value: unknown, field: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) fail(field);
  return value;
}

export function iso(value: unknown, field: string): string {
  if (typeof value !== 'string' || !ISO.test(value) || !realTimestamp(value)) fail(field);
  return value;
}

const ISO_PARTS =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.\d+)?)?(?:Z|([+-])(\d{2}):(\d{2}))$/;

/**
 * A real calendar moment, as agreed with agent-report so the two never diverge: a real month and day (leap years
 * counted), hour 00-23, minute and second 00-59 (no leap second, no 24:00), and an offset within +-14:00. `Date.parse`
 * alone rolls over (`2026-02-30` reads as March 2).
 */
export function realTimestamp(value: string): boolean {
  const m = ISO_PARTS.exec(value);
  if (!m) return false;
  const [year, month, day, hour, minute] = [m[1], m[2], m[3], m[4], m[5]].map(Number);
  const second = m[6] === undefined ? 0 : Number(m[6]);
  if (month < 1 || month > 12 || hour > 23 || minute > 59 || second > 59) return false;
  // Proleptic Gregorian by arithmetic, not Date.UTC (which reads years 0-99 as 19xx), so every year counts as written.
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const lastDay = month === 2 ? (leap ? 29 : 28) : [4, 6, 9, 11].includes(month) ? 30 : 31;
  if (day < 1 || day > lastDay) return false;
  if (m[7] !== undefined) {
    const offsetHours = Number(m[8]);
    const offsetMinutes = Number(m[9]);
    if (offsetMinutes > 59 || offsetHours * 60 + offsetMinutes > 14 * 60) return false;
  }
  return Number.isFinite(Date.parse(value));
}

export function optIso(value: unknown, field: string): string | null {
  return value === null || value === undefined ? null : iso(value, field);
}

export function bool(value: unknown, field: string, fallback: boolean): boolean {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== 'boolean') fail(field);
  return value;
}

export function list<T>(
  value: unknown,
  field: string,
  max: number,
  each: (item: unknown, at: string) => T
): T[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || value.length > max) fail(field);
  return value.map((item, index) => each(item, `${field}[${index}]`));
}

export function oneOf<T extends string>(value: unknown, field: string, allowed: readonly T[]): T {
  if (typeof value !== 'string' || !(allowed as readonly string[]).includes(value)) fail(field);
  return value as T;
}

export interface ReportLink {
  label: string;
  path: string;
}

export function link(value: unknown, field: string): ReportLink {
  const item = obj(value, field);
  return {
    label: text(item.label, `${field}.label`, 120),
    path: text(item.path, `${field}.path`, 600),
  };
}

/** A relative path inside a writer's folder: plain segments, no `..`, no absolute or Windows paths. */
const IMAGE_PATH = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}(\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}){0,5}$/;

export function isWriterFilePath(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length <= 300 &&
    IMAGE_PATH.test(value) &&
    !value.split('/').some((segment) => segment === '.' || segment === '..')
  );
}

/** Shared head of every document. */
export interface DocHead {
  writer: string;
  seq: number;
  writtenAt: string;
}

export function head(doc: Record<string, unknown>, schema: string, writer: string): DocHead {
  if (doc.schema !== schema) fail('schema');
  if (doc.writer !== writer) throw new Invalid('writer does not match its folder');
  return { writer, seq: count(doc.seq, 'seq'), writtenAt: iso(doc.writtenAt, 'writtenAt') };
}
