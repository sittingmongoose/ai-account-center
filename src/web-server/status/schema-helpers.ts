import { findSecretIn } from '../../shared/secret-policy';
export { findSecret, findSecretIn } from '../../shared/secret-policy';

/**
 * Shared validators of the agent report documents (report-schema.ts, card-schema.ts). Each
 * projection throws `Invalid` with a fixed reason; nothing here echoes a file's text.
 */

export const THREAD_SCHEMA = 'agent-reports/thread@1';
export const RUN_SCHEMA = 'agent-reports/run@1';
export const TASKS_SCHEMA = 'agent-reports/tasks@1';
export const CARD_SCHEMA = 'agent-reports/card@1';
export const ANSWER_SCHEMA = 'agent-reports/answer@1';

export { type MachineId } from '../services/dashboard-hosts';
import { MACHINE_IDS } from '../services/dashboard-hosts';
import type { MachineId } from '../services/dashboard-hosts';
export const MACHINES = MACHINE_IDS;

/** `<machine>.<pm-mail name>`, lowercase, 64 characters at most. */
const WRITER = /^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9.-]*$/;
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
    typeof value === 'string' &&
    value.length <= 64 &&
    WRITER.test(value) &&
    MACHINES.includes(value.slice(0, value.indexOf('.')) as MachineId) &&
    !value.includes('--')
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

/** A card's local id: 64 characters at most (the `LOCAL_ID` rule). */
export const CARD_LOCAL_ID_MAX = 64;
const LOCAL_ID_GRAMMAR = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

/**
 * Which rule a `<writer>--<local id>` id breaks, in fixed words, or null when it keeps them all: the
 * writer prefix (this folder's writer and `--`), then the local id's length, then its grammar. Each
 * refusal names its rule so a writer can fix the right thing; the id itself is never echoed.
 */
export function idProblem(value: unknown, writer: string, localMax: number): string | null {
  if (typeof value !== 'string' || value.length === 0) return 'id is missing or not valid';
  if (!value.startsWith(`${writer}--`))
    return `id does not name its writer: it must start with "${writer}--"`;
  const local = value.slice(writer.length + 2);
  if (local.length === 0) return 'id has no local part after its writer and "--"';
  if (local.length > localMax)
    return `id is too long: the part after "${writer}--" is ${local.length} characters, over the ${localMax} allowed`;
  if (!LOCAL_ID_GRAMMAR.test(local))
    return 'id is not valid: the part after its writer and "--" must start with a letter or digit and hold only letters, digits, ".", "_" and "-"';
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

/** A link's label: 120 characters, unless a document kind publishes another bound (run evidence: 300). */
export const LINK_LABEL_MAX = 120;

export function link(value: unknown, field: string, labelMax: number = LINK_LABEL_MAX): ReportLink {
  const item = obj(value, field);
  return {
    label: boundedText(item.label, `${field}.label`, labelMax),
    path: text(item.path, `${field}.path`, 600),
  };
}

/**
 * `text`, with a refusal that says the length rule when that is the one broken (a published
 * `maxLength`), so a writer knows to shorten it rather than look for a wrong character.
 */
export function boundedText(value: unknown, field: string, max: number): string {
  if (typeof value === 'string' && value.length > max)
    throw new Invalid(`${field} is too long: ${value.length} characters, over the ${max} allowed`);
  return text(value, field, max);
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
