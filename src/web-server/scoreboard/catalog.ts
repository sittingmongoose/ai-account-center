/**
 * The Model Scoreboard catalog (docs/scoreboard.md): `<reports>/v1/catalog/*.json`, owned by AAC and
 * edited on the share as data. Each file is `{ schema: "catalog-<kind>@1", <kind>: [...] }`. A file
 * that later fails keeps its last good copy; a catalog with no good copy shows as a Source problem
 * and the kinds it holds stay empty, never a crash. Unknown fields never reach the view; reasons are
 * fixed sentences. At server start AAC writes a packaged seed only where the share has no file.
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

const FILES = {
  'models.json': { schema: 'catalog-models@1', key: 'models' },
  'task-types.json': { schema: 'catalog-task-types@1', key: 'taskTypes' },
  'measures.json': { schema: 'catalog-measures@1', key: 'measures' },
  'uses.json': { schema: 'catalog-uses@1', key: 'uses' },
  'benchmarks.json': { schema: 'catalog-benchmarks@1', key: 'benchmarks' },
} as const;

type CatalogFileName = keyof typeof FILES;
export const CATALOG_FILES = Object.keys(FILES) as CatalogFileName[];

export type CatalogFile =
  | { kind: 'models'; models: CatalogModel[] }
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
    return {
      state: this.state,
      scannedAt: this.scannedAt,
      missingFiles: CATALOG_FILES.filter(
        (name) => !this.files.get(`catalog/${name}`) && !named.has(`catalog/${name}`)
      ),
      problems,
      minRuns: measuresFile?.minRuns ?? DEFAULT_MIN_RUNS,
      models: read('models.json', 'models')?.models ?? [],
      taskTypes: read('task-types.json', 'taskTypes')?.taskTypes ?? [],
      measures: measuresFile?.measures ?? [],
      uses: read('uses.json', 'uses')?.uses ?? [],
      benchmarks: read('benchmarks.json', 'benchmarks')?.benchmarks ?? [],
    };
  }
}

/** The catalog seeds this package ships (docs/agent-reports/catalog). */
export function packagedCatalogDir(): string {
  return path.resolve(__dirname, '../../../docs/agent-reports/catalog');
}

/**
 * Writes each packaged seed into an existing `v1/catalog` folder where the folder lists no such file
 * (temporary dot file, then rename). Never overwrites: the owner edits these as data. Resolves the
 * names it wrote; never throws.
 */
export async function seedCatalog(
  root: string,
  sourceDir: string = packagedCatalogDir()
): Promise<string[]> {
  const target = path.join(root, 'v1', 'catalog');
  const written: string[] = [];
  try {
    // List, then look (a listing refreshes the NFS view of a file another machine just wrote).
    const listed = new Set(await fs.promises.readdir(target));
    for (const name of CATALOG_FILES) {
      if (listed.has(name)) continue;
      const destination = path.join(target, name);
      const absent = await fs.promises
        .lstat(destination)
        .then(() => false)
        .catch((error: NodeJS.ErrnoException) => error.code === 'ENOENT');
      if (!absent) continue;
      const temp = path.join(target, `.${name}.tmp-${process.pid}-${Date.now()}`);
      try {
        await fs.promises.copyFile(path.join(sourceDir, name), temp);
        await fs.promises.chmod(temp, 0o644).catch(() => undefined);
        await fs.promises.rename(temp, destination);
        written.push(name);
      } catch {
        await fs.promises.rm(temp, { force: true }).catch(() => undefined);
      }
    }
  } catch {
    /* No catalog folder (share not mounted, or no permission): the Source shows it. */
  }
  return written;
}
