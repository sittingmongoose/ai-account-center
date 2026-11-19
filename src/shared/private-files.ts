/**
 * The private-file registry shared with Rust (L2, DESIGN-M3-COMPLETE section 2.2):
 * `config/private-files.json`. One row per file outside the store: its root, path, class, byte
 * cap, mode, the writer on the dashboard host (`owner`), whether the agent writes the same layout
 * on other computers (`agent`), and the route group whose `on` hands it to the Rust service
 * (`movesWith`). The Rust crate `aac-files` embeds the same file and applies the same rules.
 *
 * Owners are resolved once at start (`settlePrivateFileOwners`) and sent to the service in its
 * boot JSON, which refuses a map that differs from its own registry. TS writers then refuse a
 * row they do not own (`assertTsWritesFile`, `assertTsWritesRow`), so each file has one writer.
 * The collection group transfers its quota caches when explicitly enabled.
 */
import path from 'path';
import { ConfigError } from '../errors/error-types';

export type PrivateFileRole = 'ts' | 'rust-service' | 'agent';
export type PrivateFileClass =
  'credential' | 'setting' | 'state' | 'cache' | 'history' | 'log' | 'backup';

export interface PrivateFileRow {
  readonly id: string;
  readonly root: 'ccs' | 'share';
  /** Relative, `/`-separated; one `*` may stand for a plain file name in the last component. */
  readonly path: string;
  readonly class: PrivateFileClass;
  readonly maxBytes: number;
  /** Octal permission bits as text ("0600"). */
  readonly mode: string;
  /** The writer on the dashboard host while no route group has moved the row. */
  readonly owner: 'ts';
  readonly agent: boolean;
  readonly movesWith: string | null;
  readonly parents: 'private' | 'reference';
  readonly lease: 'file' | 'folder';
}

export const PRIVATE_FILES_PATH = path.resolve(__dirname, '../../config/private-files.json');

const KEYS = [
  'id',
  'root',
  'path',
  'class',
  'maxBytes',
  'mode',
  'owner',
  'agent',
  'movesWith',
  'parents',
  'lease',
].sort();
const CLASSES = new Set(['credential', 'setting', 'state', 'cache', 'history', 'log', 'backup']);
const MAX_ROW_BYTES = 64 * 1024 * 1024;

function idWord(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    /^[a-z][a-z0-9-]*$/.test(value) &&
    !value.endsWith('-') &&
    !value.includes('--')
  );
}

function plainPath(value: string): boolean {
  const parts = value.split('/');
  return (
    value.length > 0 &&
    parts.every(
      (part, i) =>
        part.length > 0 &&
        !part.startsWith('.') &&
        !part.endsWith('.lock') &&
        (part.match(/\*/g)?.length ?? 0) <= (i === parts.length - 1 ? 1 : 0) &&
        /^[A-Za-z0-9._*-]+$/.test(part)
    )
  );
}

/** Parse and check a registry document: the rules `aac_files::registry::parse` applies. */
export function parsePrivateFiles(value: unknown): PrivateFileRow[] {
  const fail = (why: string): never => {
    throw new ConfigError(`config/private-files.json: ${why}`);
  };
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('not an object');
  const top = value as Record<string, unknown>;
  if (Object.keys(top).sort().join(',') !== 'rows,version' || top.version !== 1)
    fail('not version 1');
  if (!Array.isArray(top.rows)) fail('no rows');
  const ids = new Set<string>();
  const places = new Set<string>();
  return (top.rows as unknown[]).map((item, index) => {
    if (!item || typeof item !== 'object' || Array.isArray(item)) return fail(`row ${index}`);
    const row = item as Record<string, unknown>;
    if (Object.keys(row).sort().join(',') !== KEYS.join(',')) fail(`row ${index} keys`);
    const mode = typeof row.mode === 'string' ? row.mode : '';
    const modeOk =
      /^0[0-7]{3}$/.test(mode) &&
      (row.root === 'ccs' ? mode === '0600' : mode === '0600' || mode === '0644');
    if (
      !idWord(row.id) ||
      ids.has(row.id) ||
      (row.root !== 'ccs' && row.root !== 'share') ||
      typeof row.path !== 'string' ||
      !plainPath(row.path) ||
      places.has(`${row.root}:${row.path}`) ||
      typeof row.class !== 'string' ||
      !CLASSES.has(row.class) ||
      !Number.isSafeInteger(row.maxBytes) ||
      (row.maxBytes as number) < 1 ||
      (row.maxBytes as number) > MAX_ROW_BYTES ||
      !modeOk ||
      row.owner !== 'ts' ||
      typeof row.agent !== 'boolean' ||
      (row.agent && row.root !== 'ccs') ||
      (row.movesWith !== null && !idWord(row.movesWith)) ||
      (row.parents !== 'private' && row.parents !== 'reference') ||
      (row.lease !== 'file' && row.lease !== 'folder')
    )
      fail(`row ${index} breaks a rule`);
    ids.add(row.id as string);
    places.add(`${row.root}:${row.path}`);
    return Object.freeze({ ...row }) as unknown as PrivateFileRow;
  });
}

export const PRIVATE_FILE_ROWS: readonly PrivateFileRow[] = Object.freeze(
  parsePrivateFiles(require(PRIVATE_FILES_PATH))
);

/**
 * Each row's writer on the dashboard host: its own `owner`, or the Rust service when the row
 * moves with a route group that is `on`.
 */
export function resolvePrivateFileOwners(
  rows: readonly PrivateFileRow[],
  groupOn: (group: string) => boolean
): Record<string, PrivateFileRole> {
  const owners: Record<string, PrivateFileRole> = {};
  for (const row of rows)
    owners[row.id] = row.movesWith !== null && groupOn(row.movesWith) ? 'rust-service' : row.owner;
  return owners;
}

let settled: Readonly<Record<string, PrivateFileRole>> | null = null;

/**
 * Resolve the owners for this process from the service's route groups; the dashboard calls this
 * once at start (the Rust supervisor), before the boot JSON is written.
 */
export function settlePrivateFileOwners(
  groupOn: (group: string) => boolean,
  rows: readonly PrivateFileRow[] = PRIVATE_FILE_ROWS
): Readonly<Record<string, PrivateFileRole>> {
  settled = Object.freeze(resolvePrivateFileOwners(rows, groupOn));
  return settled;
}

/** The settled owners (each row's own `owner` before the supervisor settles them). */
export function privateFileOwners(): Readonly<Record<string, PrivateFileRole>> {
  return settled ?? resolvePrivateFileOwners(PRIVATE_FILE_ROWS, () => false);
}

function matches(pattern: string, relative: string): boolean {
  const star = pattern.indexOf('*');
  if (star < 0) return pattern === relative;
  const head = pattern.slice(0, star);
  const tail = pattern.slice(star + 1);
  const name = relative.slice(head.length, relative.length - tail.length);
  return (
    relative.length > head.length + tail.length &&
    relative.startsWith(head) &&
    relative.endsWith(tail) &&
    !name.includes('/') &&
    !name.startsWith('.')
  );
}

/** The `ccs` row a file under the CCS folder is, if the registry lists it. */
export function privateFileRowFor(file: string, ccsDir: string): PrivateFileRow | undefined {
  const relative = path.relative(path.resolve(ccsDir), path.resolve(file));
  if (!relative || relative.startsWith('..') || path.isAbsolute(relative)) return undefined;
  const posix = relative.split(path.sep).join('/');
  // Narrow companion patterns (cooldowns and peer grants) precede the observation wildcard.
  return PRIVATE_FILE_ROWS.find((row) => row.root === 'ccs' && matches(row.path, posix));
}

/** A write TS may not make: another process owns this row now. */
export class PrivateFileOwnerError extends ConfigError {
  constructor() {
    // Never the path (CONTRACT-registry-lifecycle rule 7).
    super('Another process writes this private file now.');
    this.name = 'PrivateFileOwnerError';
  }
}

/** Whether TS writes this registered row now. */
export function tsWritesRow(id: string): boolean {
  const owner = privateFileOwners()[id];
  if (owner === undefined) throw new ConfigError(`unregistered private file row: ${id}`);
  return owner === 'ts';
}

/** Throw unless TS writes this registered row now. */
export function assertTsWritesRow(id: string): void {
  if (!tsWritesRow(id)) throw new PrivateFileOwnerError();
}

/**
 * Throw when `file` is a registered `ccs` row that another process writes now. A file the
 * registry does not list is not this rule's to refuse.
 */
export function assertTsWritesFile(file: string, ccsDir: string): void {
  const row = privateFileRowFor(file, ccsDir);
  if (row) assertTsWritesRow(row.id);
}
