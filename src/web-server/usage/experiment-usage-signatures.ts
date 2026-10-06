/**
 * Content signatures for experiment usage roots (experiment-usage-roots.ts): which tool wrote a
 * log, judged from a bounded head (or tail) of its records; whether a SQLite file is a zcode
 * database, judged from its schema pages; a content identity for zcode copies; the sandbox
 * markers synthetic fixture trees carry; and where a found log's collector should read.
 */
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { AAC_SANDBOX_MARKER } from './omp-native-usage-collector';

export type ExperimentKind = 'claude' | 'codex' | 'muse' | 'zcode';
/** Bytes read from the head (and, when the head is inconclusive, the tail) of a candidate. */
const SNIFF_BYTES = 64 * 1024;
const SNIFF_LINES = 40;

export const isText = (value: unknown): value is string =>
  typeof value === 'string' && value.length > 0;
export const isRecord = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === 'object' && !Array.isArray(value);

function parseLine(line: string): Record<string, unknown> | null {
  try {
    const value: unknown = JSON.parse(line);
    return isRecord(value) ? value : null;
  } catch {
    return null;
  }
}

/**
 * The tool whose own record signature a bounded piece of a log carries, or null. Each signature
 * needs fields only the real tool writes, so the synthetic fixtures the AAC generators write
 * (which carry just the usage fields) never match:
 * - Claude Code transcript: `sessionId`, `version` and `cwd` on a user/assistant/system record;
 * - Claude Code stream capture (`claude -p --output-format stream-json`): the `system/init`
 *   record with `claude_code_version`, or an assistant record with usage, `session_id`, `uuid`
 *   and `timestamp`;
 * - Codex rollout (`rollout-*.jsonl`): a `session_meta` record that names `originator` or
 *   `cli_version`;
 * - Muse Code session (`session.jsonl`): an event envelope with `record_type`, `recorded_at` and
 *   `payload`.
 */
export function classifyUsageLogText(text: string, fileName: string): ExperimentKind | null {
  let parsed = 0;
  for (const raw of text.split('\n')) {
    if (parsed >= SNIFF_LINES) break;
    const line = raw.trim();
    if (!line.startsWith('{') || !line.endsWith('}')) continue;
    // A Codex session_meta line can be long (instructions follow the identity fields), so its
    // prefix decides without parsing the rest.
    if (fileName.startsWith('rollout-') && /"type"\s*:\s*"session_meta"/.test(line.slice(0, 4096)))
      return /"(originator|cli_version)"\s*:\s*"/.test(line) ? 'codex' : null;
    const record = parseLine(line);
    if (!record) continue;
    parsed++;
    const type = record.type;
    if (
      (type === 'user' || type === 'assistant' || type === 'system') &&
      isText(record.sessionId) &&
      isText(record.version) &&
      isText(record.cwd)
    )
      return 'claude';
    if (type === 'system' && record.subtype === 'init' && isText(record.claude_code_version))
      return isText(record.session_id) ? 'claude' : null;
    if (
      type === 'assistant' &&
      isText(record.session_id) &&
      isText(record.uuid) &&
      isText(record.timestamp) &&
      isRecord(record.message) &&
      isRecord(record.message.usage)
    )
      return 'claude';
    if (isText(record.record_type) && record.recorded_at !== undefined && isRecord(record.payload))
      return fileName === 'session.jsonl' ? 'muse' : null;
  }
  return null;
}

/** Up to SNIFF_BYTES from the head or the tail of an open file, as raw bytes. */
function readBytes(fd: number, from: 'head' | 'tail', size: number): Buffer {
  const length = Math.min(SNIFF_BYTES, size);
  const buffer = Buffer.alloc(length);
  const position = from === 'head' ? 0 : Math.max(0, size - length);
  const read = fs.readSync(fd, buffer, 0, length, position);
  return buffer.subarray(0, read);
}

function sliceText(fd: number, from: 'head' | 'tail', size: number): string {
  let bytes = readBytes(fd, from, size);
  // A tail read starts mid-line: drop the partial first line.
  if (from === 'tail' && size > SNIFF_BYTES) bytes = bytes.subarray(bytes.indexOf(10) + 1);
  return bytes.toString('utf8');
}

/** Head first; a head without one complete record (a huge first line) falls back to the tail. */
export function classifyUsageLogFile(file: string): ExperimentKind | null {
  let fd: number | undefined;
  try {
    fd = fs.openSync(file, 'r');
    const stats = fs.fstatSync(fd);
    if (!stats.isFile() || stats.size === 0) return null;
    const name = path.basename(file);
    const kind = classifyUsageLogText(sliceText(fd, 'head', stats.size), name);
    if (kind || stats.size <= SNIFF_BYTES) return kind;
    return classifyUsageLogText(sliceText(fd, 'tail', stats.size), name);
  } catch {
    return null;
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}

/** SQLite varint at `offset`: [value, bytes used]. */
function varint(buffer: Buffer, offset: number): [number, number] {
  let value = 0;
  for (let index = 0; index < 9; index++) {
    const byte = buffer[offset + index];
    if (byte === undefined) return [value, index + 1];
    if (index === 8) return [value * 256 + byte, 9];
    value = value * 128 + (byte & 0x7f);
    if (byte < 0x80) return [value, index + 1];
  }
  return [value, 9];
}

/** Bytes a record value of this serial type occupies. */
function serialSize(type: number): number {
  if (type >= 12) return Math.floor((type - (type % 2 ? 13 : 12)) / 2);
  return [0, 1, 2, 3, 4, 6, 8, 8, 0, 0, 0, 0][type] ?? 0;
}

/**
 * The tables in a SQLite file's schema (`sqlite_master`) with their CREATE statements, read
 * straight from its b-tree pages: page 1 and, when the schema spans pages, its children (bounded
 * to 64 pages). Only each entry's `type`, `name` and the part of its `sql` stored on the page are
 * decoded. Null when the file is not a readable SQLite database.
 */
export function sqliteTables(file: string): Map<string, string> | null {
  let fd: number | undefined;
  try {
    fd = fs.openSync(file, 'r');
    const header = Buffer.alloc(100);
    if (fs.readSync(fd, header, 0, 100, 0) < 100) return null;
    if (header.toString('latin1', 0, 16) !== 'SQLite format 3\u0000') return null;
    const rawSize = header.readUInt16BE(16);
    const pageSize = rawSize === 1 ? 65536 : rawSize;
    if (pageSize < 512 || (pageSize & (pageSize - 1)) !== 0) return null;
    const names = new Map<string, string>();
    const handle = fd;
    let pages = 0;
    const visit = (page: number, depth: number): void => {
      if (depth > 4 || pages >= 64 || page < 1) return;
      pages++;
      const buffer = Buffer.alloc(pageSize);
      const read = fs.readSync(handle, buffer, 0, pageSize, (page - 1) * pageSize);
      if (read < pageSize) return;
      const start = page === 1 ? 100 : 0;
      const kind = buffer[start];
      const cells = buffer.readUInt16BE(start + 3);
      const pointers = start + (kind === 0x05 ? 12 : 8);
      for (let cell = 0; cell < cells && cell < 1000; cell++) {
        const at = buffer.readUInt16BE(pointers + cell * 2);
        if (at >= pageSize) continue;
        if (kind === 0x05) {
          visit(buffer.readUInt32BE(at), depth + 1);
          continue;
        }
        if (kind !== 0x0d) return;
        let cursor = at;
        cursor += varint(buffer, cursor)[1]; // payload length
        cursor += varint(buffer, cursor)[1]; // rowid
        const [headerLength, used] = varint(buffer, cursor);
        let typeAt = cursor + used;
        let valueAt = cursor + headerLength;
        // Columns: type, name, tbl_name, rootpage, sql. A long sql spills to overflow pages;
        // only the part on this page is kept.
        const values: string[] = [];
        for (let column = 0; column < 5 && typeAt < cursor + headerLength; column++) {
          const [serial, width] = varint(buffer, typeAt);
          typeAt += width;
          const size = serialSize(serial);
          const text = serial >= 13 && serial % 2 === 1;
          values.push(
            text ? buffer.toString('utf8', valueAt, Math.min(pageSize, valueAt + size)) : ''
          );
          valueAt += size;
        }
        if (values[0] === 'table' && values[1]) names.set(values[1], values[4] ?? '');
      }
      if (kind === 0x05) visit(buffer.readUInt32BE(start + 8), depth + 1);
    };
    visit(1, 0);
    return names;
  } catch {
    return null;
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}

/** The `model_usage` columns the zcode reader queries (analytics_usage_remote.py). */
const ZCODE_USAGE_COLUMNS = [
  'model_id',
  'provider_id',
  'started_at',
  'input_tokens',
  'output_tokens',
  'cache_read_input_tokens',
  'cache_creation_input_tokens',
];

/**
 * A zcode database: a SQLite file whose schema has a `model_usage` table with every column the
 * zcode reader queries. Another tool's table of the same name (other columns) does not match.
 */
export function isZcodeDatabase(file: string): boolean {
  const sql = sqliteTables(file)?.get('model_usage');
  return !!sql && ZCODE_USAGE_COLUMNS.every((column) => new RegExp(`\\b${column}\\b`).test(sql));
}

/**
 * Content identity of a zcode database, so byte-identical copies are read once: its size, its
 * header (which carries the write counter and page count), its first and last 64 KB and 16 pages
 * spread across it, plus the same for its write-ahead log. About 200 KB per database instead of
 * hashing it whole; two independent databases agreeing on all of it do not occur in practice.
 */
export function zcodeDigest(file: string): string {
  const hash = createHash('sha256');
  for (const part of [file, `${file}-wal`]) {
    let fd: number | undefined;
    try {
      fd = fs.openSync(part, 'r');
      const size = fs.fstatSync(fd).size;
      // An empty write-ahead log is the same as none: opening a copy read-only can create one.
      if (!size) continue;
      hash.update(`${part === file ? 'db' : 'wal'}:${size}:`);
      hash.update(readBytes(fd, 'head', size));
      hash.update(readBytes(fd, 'tail', size));
      const sample = Buffer.alloc(4096);
      for (let index = 1; index <= 16; index++) {
        const read = fs.readSync(fd, sample, 0, 4096, Math.floor((size * index) / 17));
        hash.update(sample.subarray(0, read));
      }
    } catch {
      /* No write-ahead log: same as an empty one. */
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
    }
  }
  return hash.digest('hex').slice(0, 32);
}

const DATE_PARTS = [/^\d{4}$/, /^\d{2}$/, /^\d{2}$/];

/**
 * The location a found log's collector reads. Codex and Muse keep sessions in date folders
 * (`<sessions>/YYYY/MM/DD/...`), so their root is the folder above the date (a log outside that
 * layout roots at its own folder for Codex, its session folder's parent for Muse); a Claude root
 * is the folder holding the log; a zcode root is the database itself.
 */
export function experimentRootFor(kind: ExperimentKind, file: string): string {
  if (kind === 'zcode') return file;
  if (kind === 'claude') return path.dirname(file);
  const directory = kind === 'muse' ? path.dirname(path.dirname(file)) : path.dirname(file);
  const parts = directory.split(path.sep);
  const tail = parts.slice(-3);
  if (tail.length === 3 && tail.every((part, index) => DATE_PARTS[index].test(part)))
    return parts.slice(0, -3).join(path.sep) || path.sep;
  return directory;
}

/**
 * The sandbox marker a directory holds: the generator marker (`.aac-synthetic`), a generator
 * `MANIFEST.json` with a `trees` key (sandboxes from before the marker), or `SANDBOX.md`. The
 * MANIFEST read is one bounded small file; anything unparseable fails open (not a sandbox).
 */
export function sandboxMarker(
  directory: string,
  entries: fs.Dirent[]
): 'generator' | 'readme' | null {
  let hasManifest = false;
  let hasReadme = false;
  for (const entry of entries) {
    if (!entry.isFile()) continue;
    if (entry.name === AAC_SANDBOX_MARKER) return 'generator';
    if (entry.name === 'SANDBOX.md') hasReadme = true;
    if (entry.name === 'MANIFEST.json') hasManifest = true;
  }
  if (hasManifest) {
    try {
      const text = fs.readFileSync(path.join(directory, 'MANIFEST.json'), 'utf8');
      if (text.length <= 64 * 1024) {
        const parsed: unknown = JSON.parse(text);
        if (isRecord(parsed) && isRecord(parsed.trees)) return 'generator';
      }
    } catch {
      /* Not a generator manifest. */
    }
  }
  return hasReadme ? 'readme' : null;
}
