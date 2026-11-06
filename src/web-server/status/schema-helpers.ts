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
const PM_MAIL_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
export const OPTION_KEY = /^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$/;
/** Control characters other than tab and newline, and bidirectional overrides. */
const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]/;
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d{1,9})?)?(Z|[+-]\d{2}:\d{2})$/;

export function isWriterName(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 64 && WRITER.test(value);
}

export function writerMachine(writer: string): MachineId {
  return writer.slice(0, writer.indexOf('.')) as MachineId;
}

export function isPmMailName(value: unknown): value is string {
  return typeof value === 'string' && PM_MAIL_NAME.test(value);
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
const SECRET_PATTERNS: ReadonlyArray<readonly [string, RegExp]> = [
  ['a private key', /-----BEGIN [A-Z ]*PRIVATE KEY-----/],
  ['an API key', /\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}/],
  ['a GitHub token', /\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})/],
  ['a Slack token', /\bxox[abposr]-[A-Za-z0-9-]{10,}/],
  ['an AWS key', /\bAKIA[0-9A-Z]{16}\b/],
  ['a Google API key', /\bAIza[0-9A-Za-z_-]{35}/],
  ['a JSON web token', /\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/],
  ['a bearer token', /\bBearer\s+[A-Za-z0-9._~+/-]{20,}/i],
  [
    'a credential field',
    /"(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)"\s*:\s*"[^"]{6,}"/i,
  ],
  [
    'a credential in a URL',
    /[?&](?:access_token|token|api_key|apikey|key|sig|signature|password|code)=[^&\s"]{8,}/i,
  ],
  ['an email address', /[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/],
];

/** The kind of secret a text contains, or null. */
export function findSecret(text: string): string | null {
  for (const [kind, pattern] of SECRET_PATTERNS) if (pattern.test(text)) return kind;
  return null;
}

export type Parsed<T> = { ok: true; value: T } | { ok: false; reason: string };

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
  if (typeof value !== 'string' || !ISO.test(value) || !Number.isFinite(Date.parse(value)))
    fail(field);
  return value;
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
