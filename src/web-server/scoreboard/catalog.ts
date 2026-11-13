/**
 * The Model Scoreboard catalog (docs/scoreboard.md): `<reports>/v1/catalog/*.json`, owned by AAC and
 * edited on the share as data. Each file is `{ schema: "catalog-<kind>@1", <kind>: [...] }`, except
 * `model-aliases.json`, which is `{ schema: "agent-reports/model-aliases@1",
 * aliases: { <written id>: <canonical id> }, effortSuffixes: ["-high", ...] }` (see model-identity.ts).
 * A file that later fails keeps its last good copy; a catalog with no good copy shows as a Source
 * problem and the kinds it holds stay empty, never a crash. Unknown fields never reach the view;
 * reasons are fixed sentences. At server start AAC writes a packaged seed where the share has no
 * file, and adds to an existing `models.json` or `model-aliases.json` only the packaged entries it
 * lacks by id (the dashboard publishing step for the catalog; `seedCatalog`).
 */
import fs from 'fs';
import path from 'path';
import {
  isTransientShareError,
  nodeReaderFs,
  type ReaderFs,
  type ReportProblem,
} from '../status/agent-reports-reader';
import {
  count,
  fail,
  Invalid,
  jsonFromBytes,
  list,
  obj,
  oneOf,
  optIso,
  optText,
  text,
  type Parsed,
} from '../status/schema-helpers';
import { byFile, ShareFiles } from './share-files';

export const CATALOG_CAP_BYTES = 256 * 1024;
export const DEFAULT_MIN_RUNS = 5;

export const TASK_UNITS = ['record', 'file', 'item', 'word', 'task'] as const;
export type TaskUnit = (typeof TASK_UNITS)[number];

export interface CatalogModel {
  model: string;
  label: string;
  family: string | null;
  runtimes: string[];
  efforts: string[];
  prices: {
    input: number | null;
    output: number | null;
    cachedInput: number | null;
    unit: string | null;
    asOf: string | null;
  } | null;
  notes: string | null;
}

export interface CatalogTaskType {
  id: string;
  label: string;
  description: string | null;
  unit: TaskUnit;
  measures: string[];
  byProcess: boolean;
}

export interface CatalogMeasure {
  id: string;
  label: string;
  better: 'lower' | 'higher';
  unit: string;
  description: string | null;
  metric: { of: string; per: string } | null;
  preliminaryUnless: string | null;
}

export interface CatalogUse {
  id: string;
  label: string;
  description: string | null;
  taskTypes: string[];
  weights: Record<string, number>;
}

export interface CatalogBenchmark {
  id: string;
  label: string;
  taskType: string | null;
  description: string | null;
  items: number | null;
  scoring: string | null;
}

/** The shared alias table: written model ids to canonical `models.json` ids, plus effort suffixes. */
export interface ModelAliases {
  aliases: Record<string, string>;
  effortSuffixes: string[];
}

const ID = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,119}$/;
const METRIC = /^[A-Za-z][A-Za-z0-9_]{0,63}$/;

function id(value: unknown, field: string): string {
  if (typeof value !== 'string' || !ID.test(value)) fail(field);
  return value;
}

function metricName(value: unknown, field: string): string {
  if (typeof value !== 'string' || !METRIC.test(value)) fail(field);
  return value;
}

function ids(value: unknown, field: string): string[] {
  return list(value, field, 100, id);
}

function flag(value: unknown, field: string): boolean {
  if (value === undefined || value === null) return false;
  if (typeof value !== 'boolean') fail(field);
  return value;
}

function price(value: unknown, field: string): number | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) fail(field);
  return value;
}

function noRepeats<T extends { id: string }>(rows: T[], what: string): T[] {
  if (new Set(rows.map((row) => row.id)).size !== rows.length)
    throw new Invalid(`${what} repeat an id`);
  return rows;
}

function models(value: unknown): CatalogModel[] {
  const rows = list(value, 'models', 500, (item, at) => {
    const row = obj(item, at);
    const prices = row.prices === undefined || row.prices === null ? null : obj(row.prices, at);
    return {
      model: id(row.model, `${at}.model`),
      label: text(row.label, `${at}.label`, 120),
      family: optText(row.family, `${at}.family`, 120),
      runtimes: ids(row.runtimes, `${at}.runtimes`),
      efforts: ids(row.efforts, `${at}.efforts`),
      prices: prices && {
        input: price(prices.input, `${at}.prices.input`),
        output: price(prices.output, `${at}.prices.output`),
        cachedInput: price(prices.cachedInput, `${at}.prices.cachedInput`),
        unit: optText(prices.unit, `${at}.prices.unit`, 60),
        asOf: optIso(prices.asOf, `${at}.prices.asOf`),
      },
      notes: optText(row.notes, `${at}.notes`, 500),
    };
  });
  if (new Set(rows.map((row) => row.model)).size !== rows.length)
    throw new Invalid('models repeat a model');
  return rows;
}

function taskTypes(value: unknown): CatalogTaskType[] {
  return noRepeats(
    list(value, 'taskTypes', 200, (item, at) => {
      const row = obj(item, at);
      return {
        id: id(row.id, `${at}.id`),
        label: text(row.label, `${at}.label`, 120),
        description: optText(row.description, `${at}.description`, 1000),
        unit: oneOf(row.unit, `${at}.unit`, TASK_UNITS),
        measures: ids(row.measures, `${at}.measures`),
        byProcess: flag(row.byProcess, `${at}.byProcess`),
      };
    }),
    'taskTypes'
  );
}

function measures(value: unknown): CatalogMeasure[] {
  return noRepeats(
    list(value, 'measures', 200, (item, at) => {
      const row = obj(item, at);
      const metric = row.metric === undefined || row.metric === null ? null : obj(row.metric, at);
      return {
        id: id(row.id, `${at}.id`),
        label: text(row.label, `${at}.label`, 120),
        better: oneOf(row.better, `${at}.better`, ['lower', 'higher'] as const),
        unit: text(row.unit, `${at}.unit`, 40),
        description: optText(row.description, `${at}.description`, 1000),
        metric: metric && {
          of: metricName(metric.of, `${at}.metric.of`),
          per: metricName(metric.per, `${at}.metric.per`),
        },
        preliminaryUnless:
          row.preliminaryUnless === undefined || row.preliminaryUnless === null
            ? null
            : metricName(row.preliminaryUnless, `${at}.preliminaryUnless`),
      };
    }),
    'measures'
  );
}

function uses(value: unknown): CatalogUse[] {
  return noRepeats(
    list(value, 'uses', 100, (item, at) => {
      const row = obj(item, at);
      const raw = obj(row.weights, `${at}.weights`);
      const weights: Record<string, number> = {};
      for (const [name, weight] of Object.entries(raw)) {
        const key = id(name, `${at}.weights`);
        if (typeof weight !== 'number' || !Number.isFinite(weight) || weight < 0)
          fail(`${at}.weights`);
        weights[key] = weight;
      }
      const types = list(row.taskTypes, `${at}.taskTypes`, 200, (type, field) =>
        type === '*' ? '*' : id(type, field)
      );
      return {
        id: id(row.id, `${at}.id`),
        label: text(row.label, `${at}.label`, 120),
        description: optText(row.description, `${at}.description`, 1000),
        taskTypes: types,
        weights,
      };
    }),
    'uses'
  );
}

function benchmarks(value: unknown): CatalogBenchmark[] {
  return noRepeats(
    list(value, 'benchmarks', 200, (item, at) => {
      const row = obj(item, at);
      return {
        id: id(row.id, `${at}.id`),
        label: text(row.label, `${at}.label`, 120),
        taskType: row.taskType === undefined || row.taskType === null ? null : id(row.taskType, at),
        description: optText(row.description, `${at}.description`, 1000),
        items: row.items === undefined || row.items === null ? null : count(row.items, at),
        scoring: optText(row.scoring, `${at}.scoring`, 1000),
      };
    }),
    'benchmarks'
  );
}

/**
 * An effort suffix: one leading dash, then a lowercase word (`-high`, `-xhigh`, ...). The dash is
 * part of the stored form so stripping is exact; the effort value is the suffix without the dash.
 */
const SUFFIX = /^-[a-z][a-z0-9-]{0,15}$/;

function modelAliases(doc: Record<string, unknown>): ModelAliases {
  const raw = obj(doc.aliases, 'aliases');
  const entries = Object.entries(raw);
  if (entries.length > 500) fail('aliases');
  const aliases: Record<string, string> = {};
  for (const [written, canonical] of entries) {
    if (typeof written !== 'string' || written.length === 0 || written.length > 120)
      fail('aliases');
    if (typeof canonical !== 'string' || canonical.length === 0 || canonical.length > 120)
      fail('aliases');
    text(written, 'aliases', 120);
    text(canonical, 'aliases', 120);
    aliases[written] = canonical;
  }
  const suffixes = list(doc.effortSuffixes, 'effortSuffixes', 32, (item, at) => {
    const suffix = text(item, at, 16);
    if (!SUFFIX.test(suffix)) fail(at);
    return suffix;
  });
  return { aliases, effortSuffixes: [...new Set(suffixes)] };
}

const FILES = {
  'models.json': { schema: 'catalog-models@1', key: 'models' },
  'model-aliases.json': { schema: 'agent-reports/model-aliases@1', key: 'aliases' },
  'task-types.json': { schema: 'catalog-task-types@1', key: 'taskTypes' },
  'measures.json': { schema: 'catalog-measures@1', key: 'measures' },
  'uses.json': { schema: 'catalog-uses@1', key: 'uses' },
  'benchmarks.json': { schema: 'catalog-benchmarks@1', key: 'benchmarks' },
} as const;

type CatalogFileName = keyof typeof FILES;
export const CATALOG_FILES = Object.keys(FILES) as CatalogFileName[];

export type CatalogFile =
  | { kind: 'models'; models: CatalogModel[] }
  | { kind: 'aliases'; aliases: Record<string, string>; effortSuffixes: string[] }
  | { kind: 'taskTypes'; taskTypes: CatalogTaskType[] }
  | { kind: 'measures'; minRuns: number; measures: CatalogMeasure[] }
  | { kind: 'uses'; uses: CatalogUse[] }
  | { kind: 'benchmarks'; benchmarks: CatalogBenchmark[] };

/** Bytes of one catalog file -> its projected content. Every refusal is a fixed reason. */
export function parseCatalogFile(name: CatalogFileName, bytes: Buffer): Parsed<CatalogFile> {
  const raw = jsonFromBytes(bytes, CATALOG_CAP_BYTES);
  if (!raw.ok) return raw;
  const { schema, key } = FILES[name];
  try {
    const doc = obj(raw.value, 'document');
    if (doc.schema !== schema) fail('schema');
    if (key === 'aliases') {
      const parsed = modelAliases(doc);
      return {
        ok: true,
        value: { kind: 'aliases', aliases: parsed.aliases, effortSuffixes: parsed.effortSuffixes },
      };
    }
    const rows = doc[key];
    if (!Array.isArray(rows)) fail(key);
    switch (key) {
      case 'models':
        return { ok: true, value: { kind: key, models: models(rows) } };
      case 'taskTypes':
        return { ok: true, value: { kind: key, taskTypes: taskTypes(rows) } };
      case 'measures': {
        const minRuns =
          doc.minRuns === undefined ? DEFAULT_MIN_RUNS : count(doc.minRuns, 'minRuns');
        if (minRuns < 1) fail('minRuns');
        return { ok: true, value: { kind: key, minRuns, measures: measures(rows) } };
      }
      case 'uses':
        return { ok: true, value: { kind: key, uses: uses(rows) } };
      default:
        return { ok: true, value: { kind: 'benchmarks', benchmarks: benchmarks(rows) } };
    }
  } catch (error) {
    if (error instanceof Invalid) return { ok: false, reason: error.message };
    return { ok: false, reason: 'could not be read' };
  }
}

export interface CatalogState {
  /** missing: no `v1/catalog` folder; error: the folder could not be listed. */
  state: 'ok' | 'missing' | 'error';
  scannedAt: number | null;
  /** Catalog files with no good copy and no named problem. */
  missingFiles: string[];
  problems: ReportProblem[];
  minRuns: number;
  models: CatalogModel[];
  /** The alias table (`model-aliases.json`), empty when the file has no good copy. */
  aliases: Record<string, string>;
  effortSuffixes: string[];
  taskTypes: CatalogTaskType[];
  measures: CatalogMeasure[];
  uses: CatalogUse[];
  benchmarks: CatalogBenchmark[];
}

export class CatalogReader {
  private readonly files: ShareFiles<CatalogFile>;
  private state: CatalogState['state'] = 'missing';
  private scannedAt: number | null = null;
  private scanning: Promise<void> | null = null;

  constructor(private readonly options: { root: () => string; now?: () => number; fs?: ReaderFs }) {
    this.files = new ShareFiles<CatalogFile>({
      fs: () => this.options.fs ?? nodeReaderFs,
      now: () => this.now(),
      capBytes: CATALOG_CAP_BYTES,
      parse: (bytes, file) => parseCatalogFile(path.basename(file) as CatalogFileName, bytes),
    });
  }

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  /** One rescan; overlapping calls share it. Never rejects. */
  scan(): Promise<void> {
    this.scanning ??= this.runScan()
      .catch(() => {
        this.state = 'error';
        this.scannedAt = this.now();
      })
      .finally(() => {
        this.scanning = null;
      });
    return this.scanning;
  }

  private async runScan(): Promise<void> {
    const dir = path.join(this.options.root(), 'v1', 'catalog');
    const seen = new Set<string>();
    let listed: Set<string>;
    try {
      const entries = await (this.options.fs ?? nodeReaderFs).readdir(dir);
      listed = new Set(entries.map((entry) => entry.name));
      this.state = 'ok';
    } catch (error) {
      const code = (error as NodeJS.ErrnoException | null)?.code;
      if (code !== 'ENOENT' && isTransientShareError(error)) return;
      this.state = code === 'ENOENT' || code === 'ENOTDIR' ? 'missing' : 'error';
      // A folder that cannot be listed withdraws nothing; a removed one does, after the grace.
      if (this.state === 'error') this.files.keep('catalog/', seen);
      this.files.sweep(seen);
      this.scannedAt = this.now();
      return;
    }
    for (const name of CATALOG_FILES)
      if (listed.has(name)) await this.files.visit(path.join(dir, name), `catalog/${name}`, seen);
    this.files.sweep(seen);
    this.scannedAt = this.now();
  }

  snapshot(): CatalogState {
    const read = <K extends CatalogFile['kind']>(name: CatalogFileName, kind: K) => {
      const doc = this.files.get(`catalog/${name}`)?.doc;
      return doc?.kind === kind ? (doc as Extract<CatalogFile, { kind: K }>) : null;
    };
    const problems = byFile(this.files.problems());
    const named = new Set(problems.map((problem) => problem.file));
    const measuresFile = read('measures.json', 'measures');
    const aliasesFile = read('model-aliases.json', 'aliases');
    return {
      state: this.state,
      scannedAt: this.scannedAt,
      missingFiles: CATALOG_FILES.filter(
        (name) => !this.files.get(`catalog/${name}`) && !named.has(`catalog/${name}`)
      ),
      problems,
      minRuns: measuresFile?.minRuns ?? DEFAULT_MIN_RUNS,
      models: read('models.json', 'models')?.models ?? [],
      aliases: aliasesFile?.aliases ?? {},
      effortSuffixes: aliasesFile?.effortSuffixes ?? [],
      taskTypes: read('task-types.json', 'taskTypes')?.taskTypes ?? [],
      measures: measuresFile?.measures ?? [],
      uses: read('uses.json', 'uses')?.uses ?? [],
      benchmarks: read('benchmarks.json', 'benchmarks')?.benchmarks ?? [],
    };
  }
}

/** A seed that could not be written: the file, and the system's error code (never its message). */
export interface SeedFailure {
  file: string;
  code: string;
}

function seedErrorCode(error: unknown): string {
  const code = (error as NodeJS.ErrnoException | null)?.code;
  return code && /^[A-Z0-9_]+$/.test(code) ? code : 'failed';
}

/** The catalog seeds this package ships (docs/agent-reports/catalog). */
export function packagedCatalogDir(): string {
  return path.resolve(__dirname, '../../../docs/agent-reports/catalog');
}

/**
 * The share files the seeder extends by id when they already exist: the member holding the entries.
 * `models.json` gains the packaged models whose `model` it lacks, `model-aliases.json` the packaged
 * `aliases` keys it lacks. The other catalog files are only ever seeded whole.
 */
const APPEND_BY_ID: Partial<Record<CatalogFileName, 'models' | 'aliases'>> = {
  'models.json': 'models',
  'model-aliases.json': 'aliases',
};

/** Attempts at an append while another writer keeps changing the share file. */
const APPEND_ATTEMPTS = 3;

/** The ids a parsed file holds, for the files extended by id. */
function entryIds(file: CatalogFile): string[] {
  if (file.kind === 'models') return file.models.map((row) => row.model);
  if (file.kind === 'aliases') return Object.keys(file.aliases);
  return [];
}

/** Offsets of a top-level member's opening and closing bracket in a JSON object's text. */
interface MemberSpan {
  open: number;
  close: number;
}

/**
 * Where the top-level member `key` holds its array or object in `text` (text that parses as a JSON object).
 * Null when the member is missing, named more than once, or holds no array or object.
 */
export function memberSpan(text: string, key: string): MemberSpan | null {
  let depth = 0;
  let afterColon = false;
  let lastKey: string | null = null;
  let named = 0;
  let span: MemberSpan | null = null;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < text.length && text[j] !== '"') j += text[j] === '\\' ? 2 : 1;
      if (depth === 1 && afterColon) afterColon = false;
      else if (depth === 1) {
        lastKey = JSON.parse(text.slice(i, j + 1)) as string;
        if (lastKey === key) named++;
      }
      i = j;
    } else if (depth === 1 && c === ':') {
      afterColon = true;
    } else if (depth === 1 && c === ',') {
      afterColon = false;
    } else if (c === '{' || c === '[') {
      if (depth === 1 && afterColon && lastKey === key) span = { open: i, close: -1 };
      if (depth === 1) afterColon = false;
      depth++;
    } else if (c === '}' || c === ']') {
      depth--;
      if (depth === 1 && span && span.close < 0) span.close = i;
    }
  }
  return named === 1 && span && span.close > span.open ? span : null;
}

/**
 * `text` with `entries` added at the end of the member at `span`, in the member's own layout (its
 * indentation and line ending, or one line when the member is written on one). Every byte already in
 * `text` stays as it was; only the new entries (and the comma after the last old one) are inserted.
 */
function appendToMember(
  text: string,
  span: MemberSpan,
  entries: Array<{ wide: string; narrow: string }>
): string {
  const eol = text.includes('\r\n') ? '\r\n' : '\n';
  const indented = (entry: string, indent: string) =>
    indent + entry.split('\n').join(`${eol}${indent}`);
  const inner = text.slice(span.open + 1, span.close);
  if (inner.trim() === '') {
    const lineStart = text.lastIndexOf('\n', span.open) + 1;
    const outer = /^[ \t]*/.exec(text.slice(lineStart))?.[0] ?? '';
    const body = entries.map((entry) => indented(entry.wide, `${outer}  `)).join(`,${eol}`);
    return `${text.slice(0, span.open + 1)}${eol}${body}${eol}${outer}${text.slice(span.close)}`;
  }
  const first = span.open + 1 + (inner.length - inner.trimStart().length);
  const newline = text.lastIndexOf('\n', first);
  let last = span.close - 1;
  while (/\s/.test(text[last])) last--;
  const insert =
    newline > span.open
      ? entries
          .map((entry) => `,${eol}${indented(entry.wide, text.slice(newline + 1, first))}`)
          .join('')
      : entries.map((entry) => `, ${entry.narrow}`).join('');
  return `${text.slice(0, last + 1)}${insert}${text.slice(last + 1)}`;
}

/** The packaged entries a share file lacks, by id, in the packaged order, rendered for appending. */
function missingEntries(
  member: 'models' | 'aliases',
  seed: Record<string, unknown>,
  have: ReadonlySet<string>
): Array<{ id: string; wide: string; narrow: string }> {
  if (member === 'models') {
    const rows = Array.isArray(seed.models) ? (seed.models as Array<Record<string, unknown>>) : [];
    return rows
      .filter((row) => typeof row.model === 'string' && !have.has(row.model))
      .map((row) => ({
        id: row.model as string,
        wide: JSON.stringify(row, null, 2),
        narrow: JSON.stringify(row),
      }));
  }
  const aliases = (seed.aliases ?? {}) as Record<string, unknown>;
  return Object.entries(aliases)
    .filter(([written]) => !have.has(written))
    .map(([written, canonical]) => {
      const entry = `${JSON.stringify(written)}: ${JSON.stringify(canonical)}`;
      return { id: written, wide: entry, narrow: entry };
    });
}

/** The share file after the append, or a reason it cannot be made: never a file that reads differently. */
function appendedText(
  name: CatalogFileName,
  member: 'models' | 'aliases',
  shareText: string,
  shareIds: string[],
  entries: Array<{ id: string; wide: string; narrow: string }>
): string | null {
  const span = memberSpan(shareText, member);
  if (!span) return null;
  const next = appendToMember(shareText, span, entries);
  const parsed = parseCatalogFile(name, Buffer.from(next, 'utf8'));
  if (!parsed.ok) return null;
  const nextIds = entryIds(parsed.value);
  const expected = new Set([...shareIds, ...entries.map((entry) => entry.id)]);
  if (nextIds.length !== expected.size || nextIds.some((entryId) => !expected.has(entryId)))
    return null;
  return next;
}

/**
 * Adds to an existing share file the packaged entries it lacks, by id; never changes or removes an entry
 * already there. Writes a temporary dot file, then renames it over the share file, only if the share file
 * still holds what was read (else it reads again, up to three times). A share file that is not valid is
 * left alone and named (`invalid`), as is one the entries cannot be added to (`unmergeable`).
 */
async function appendMissing(
  target: string,
  name: CatalogFileName,
  member: 'models' | 'aliases',
  found: fs.Stats,
  sourceDir: string,
  onFailure: (failure: SeedFailure) => void
): Promise<boolean> {
  const destination = path.join(target, name);
  if (!found.isFile()) {
    onFailure({ file: name, code: 'not-a-file' });
    return false;
  }
  const seedBytes = await fs.promises.readFile(path.join(sourceDir, name)).catch(() => null);
  if (!seedBytes || !parseCatalogFile(name, seedBytes).ok) {
    onFailure({ file: name, code: 'invalid-seed' });
    return false;
  }
  // It parsed above, so it is a JSON object.
  const seed = JSON.parse(seedBytes.toString('utf8')) as Record<string, unknown>;
  for (let attempt = 0; attempt < APPEND_ATTEMPTS; attempt++) {
    const temp = path.join(target, `.${name}.tmp-${process.pid}-${Date.now()}-${attempt}`);
    try {
      const before = await fs.promises.readFile(destination);
      const share = parseCatalogFile(name, before);
      if (!share.ok) {
        onFailure({ file: name, code: 'invalid' });
        return false;
      }
      const shareIds = entryIds(share.value);
      const entries = missingEntries(member, seed, new Set(shareIds));
      if (entries.length === 0) return false;
      const next = appendedText(name, member, before.toString('utf8'), shareIds, entries);
      if (next === null) {
        onFailure({ file: name, code: 'unmergeable' });
        return false;
      }
      await fs.promises.writeFile(temp, next, { encoding: 'utf8', flag: 'wx' });
      await fs.promises.chmod(temp, found.mode & 0o777).catch(() => undefined);
      // Another writer (an owner's edit, another machine's start) since the read: read again.
      const now = await fs.promises.readFile(destination);
      if (!now.equals(before)) continue;
      await fs.promises.rename(temp, destination);
      return true;
    } catch (error) {
      onFailure({ file: name, code: seedErrorCode(error) });
      return false;
    } finally {
      await fs.promises.rm(temp, { force: true }).catch(() => undefined);
    }
  }
  onFailure({ file: name, code: 'busy' });
  return false;
}

/**
 * Publishes the packaged catalog into an existing `v1/catalog` folder. A file the folder lacks is
 * written whole (temporary dot file, then a hard link, which fails on a file that appeared meanwhile).
 * `models.json` and `model-aliases.json` that exist gain only the packaged entries they lack, by id
 * (`appendMissing`): share edits win, and nothing on the share is changed or removed. Resolves the
 * names it wrote; never throws.
 */
export async function seedCatalog(
  root: string,
  sourceDir: string = packagedCatalogDir(),
  onFailure: (failure: SeedFailure) => void = () => undefined
): Promise<string[]> {
  const target = path.join(root, 'v1', 'catalog');
  const written: string[] = [];
  try {
    // List, then look (a listing refreshes the NFS view of a file another machine just wrote).
    await fs.promises.readdir(target);
    for (const name of CATALOG_FILES) {
      const destination = path.join(target, name);
      const found = await fs.promises.lstat(destination).catch((error: NodeJS.ErrnoException) => {
        if (error.code === 'ENOENT') return null;
        onFailure({ file: name, code: seedErrorCode(error) });
        return undefined;
      });
      if (found === undefined) continue;
      if (found !== null) {
        const member = APPEND_BY_ID[name];
        if (member && (await appendMissing(target, name, member, found, sourceDir, onFailure)))
          written.push(name);
        continue;
      }
      const temp = path.join(target, `.${name}.tmp-${process.pid}-${Date.now()}`);
      try {
        await fs.promises.copyFile(path.join(sourceDir, name), temp);
        await fs.promises.chmod(temp, 0o644).catch(() => undefined);
        // `link`, not `rename`: it fails on a file another machine wrote since the look above.
        await fs.promises.link(temp, destination);
        written.push(name);
      } catch (error) {
        // Already there: the file on the share stands. Anything else leaves the file missing.
        if ((error as NodeJS.ErrnoException | null)?.code !== 'EEXIST')
          onFailure({ file: name, code: seedErrorCode(error) });
      } finally {
        await fs.promises.rm(temp, { force: true }).catch(() => undefined);
      }
    }
  } catch {
    /* No catalog folder (share not mounted, or no permission): the Source shows it. */
  }
  return written;
}
