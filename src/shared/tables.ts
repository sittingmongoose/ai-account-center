/**
 * The shared provider, tool and host tables (`config/tables/*.json`). TypeScript, the Rust
 * crate `aac-providers` (generated from the same files by its build.rs) and the drift tests
 * (`tests/spec/tables.test.ts`, `cargo test -p aac-providers`) read these files; every list
 * exported here is a view of them, never a copy. `computers.json` stays the computer table
 * (`dashboard-hosts.ts`); `hosts.json` holds only the frozen storage allowlist.
 *
 * The files ship with the package (`config/tables/`) and are read once at load, like
 * `computers.json`. Their types come from a type-only import, so object keys stay literal.
 */
import path from 'path';

export type ProviderTable = typeof import('../../config/tables/providers.json');
export type ToolTable = typeof import('../../config/tables/tools.json');
export type HostTable = typeof import('../../config/tables/hosts.json');
export type AntigravityTable = typeof import('../../config/tables/antigravity-columns.json');

/** Every id in the provider table, dashboard providers first, in table order. */
export type TableProviderId = keyof ProviderTable['providers'];
export type TableProviderRow = ProviderTable['providers'][TableProviderId];
export type TableAnalyticsTool = keyof ToolTable['analyticsTools'];
export type TableScannerOnlyKind = keyof ToolTable['scannerOnlyKinds'];
export type TableUsageLogTool = keyof ToolTable['usageLogTools'];
export type AntigravityColumn = AntigravityTable['columns'][number];

export const TABLES_DIR = path.resolve(__dirname, '../../config/tables');

function frozen<T>(value: T): T {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) frozen(child);
    Object.freeze(value);
  }
  return value;
}

function load<T>(name: string): T {
  return frozen(structuredClone(require(path.join(TABLES_DIR, name)) as T));
}

export const PROVIDER_TABLE: ProviderTable = load('providers.json');
export const TOOL_TABLE: ToolTable = load('tools.json');
export const HOST_TABLE: HostTable = load('hosts.json');
export const ANTIGRAVITY_TABLE: AntigravityTable = load('antigravity-columns.json');

/** The Antigravity pool columns in table order: a view of the table, never a copy. */
export const ANTIGRAVITY_COLUMNS: readonly AntigravityColumn[] = ANTIGRAVITY_TABLE.columns;

/** The provider rows in table order (`Object.entries` keeps it: no id is an integer key). */
export const PROVIDER_ROWS: ReadonlyArray<readonly [TableProviderId, TableProviderRow]> =
  Object.freeze(
    Object.entries(PROVIDER_TABLE.providers) as Array<[TableProviderId, TableProviderRow]>
  );

/** The ids of the rows a predicate keeps, in table order. */
export function providerIds(keep: (row: TableProviderRow) => boolean): TableProviderId[] {
  return PROVIDER_ROWS.filter(([, row]) => keep(row)).map(([id]) => id);
}

/** The table's own row for an id, or undefined for an id the table does not list. */
export function providerRow(id: string): TableProviderRow | undefined {
  return Object.prototype.hasOwnProperty.call(PROVIDER_TABLE.providers, id)
    ? PROVIDER_TABLE.providers[id as TableProviderId]
    : undefined;
}

/**
 * Compile-time drift checks: `Same<A, B>` is `true` only when both unions hold the same
 * members, so a one-sided edit of a JSON key or of a hand-written union fails `tsc`.
 */
export type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
/** An exported `Assert<Same<...>>` alias is the check: it only compiles when its argument is `true`. */
export type Assert<T extends true> = T;
