/**
 * L4 drift tests (DESIGN-M3-COMPLETE 2.4): config/tables/*.json is the one provider, tool and
 * host table. Rust generates its constants from it (crates/aac-providers, whose `cargo test`
 * binds the Rust hand lists outside the workspace). Here: every remaining hand list (the page's
 * JS and the native bars) equals its projection of the table, so a one-sided edit of either side
 * fails.
 */
import { describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { pathToFileURL } from 'url';

const root = path.resolve(import.meta.dir, '../..');
const read = (relative: string) => fs.readFileSync(path.join(root, relative), 'utf8');
const json = (relative: string) => JSON.parse(read(relative));

interface Row {
  label: string;
  longLabel: string;
  dashboard: boolean;
  card: boolean;
  keyStore: boolean;
  analytics: { order: number; label: string } | null;
  page: { signIn: string; multi: boolean } | null;
  failover: { label: string; kinds: string[] };
  quota: boolean;
}
const providerTable = json('config/tables/providers.json');
const toolTable = json('config/tables/tools.json');
const hostTable = json('config/tables/hosts.json');
const rows = Object.entries(providerTable.providers) as Array<[string, Row]>;
const ids = (keep: (row: Row) => boolean) => rows.filter(([, row]) => keep(row)).map(([id]) => id);
const label = (id: string) => (providerTable.providers[id] as Row).label;
const dashboard = ids((row) => row.dashboard);
const analyticsRows = rows
  .filter(([, row]) => row.analytics)
  .sort((a, b) => a[1].analytics!.order - b[1].analytics!.order);
const analyticsOrder = analyticsRows.map(([id]) => id);
const analyticsTools = Object.keys(toolTable.analyticsTools);
const scannerKinds = [...analyticsTools, ...Object.keys(toolTable.scannerOnlyKinds)];
const toolLabel = (id: string): string =>
  toolTable.analyticsTools[id]?.label ??
  toolTable.scannerOnlyKinds[id]?.label ??
  toolTable.usageLogTools[id]?.label;

/** The quoted literals from the one occurrence of `anchor` up to the next `end`. */
function literals(text: string, anchor: string, end: string): string[] {
  expect(text.split(anchor).length - 1).toBe(1);
  const from = text.indexOf(anchor) + anchor.length;
  const body = text.slice(from, text.indexOf(end, from));
  return [...body.matchAll(/'([^']*)'|"([^"]*)"/g)].map((m) => m[1] ?? m[2]);
}

const load = (relative: string, base = root) =>
  import(pathToFileURL(path.join(base, relative)).href);

describe('the tables themselves', () => {
  it('keep JS file order: every id is lowercase words joined by hyphens', () => {
    const id = /^[a-z][a-z0-9]*(?:-[a-z][a-z0-9]*)*$/;
    for (const table of [
      providerTable.providers,
      toolTable.analyticsTools,
      toolTable.scannerOnlyKinds,
      toolTable.usageLogTools,
    ])
      for (const key of Object.keys(table)) expect(key).toMatch(id);
  });

  it('hold only computers.json computers in the frozen storage allowlist', () => {
    const computers = json('web-dashboard/public/computers.json');
    for (const host of hostTable.legacyStorageHosts) expect(Object.keys(computers)).toContain(host);
    // An immutable compatibility list (d5ec0fc64): its readers reject later hosts, so it is
    // pinned rather than derived from the computer registry.
    expect(hostTable.legacyStorageHosts).toEqual(['ubuntu', 'mac', 'windows', 'nas1']);
  });
});

describe("the page's JS lists equal the table", () => {
  it('view-model.mjs registry and card providers', async () => {
    const view = await load('web-dashboard/public/view-model.mjs');
    expect(view.PROVIDER_REGISTRY).toEqual(
      rows
        .filter(([, row]) => row.page)
        .map(([id, row]) => ({
          id,
          label: row.label,
          longLabel: row.longLabel,
          signIn: row.page!.signIn,
          multi: row.page!.multi,
        }))
    );
    const source = read('web-dashboard/public/view-model.mjs');
    expect(literals(source, 'const CARD_PROVIDERS = [', ']')).toEqual(ids((row) => row.card));
  });

  it('labels, key-store providers and Analytics order', async () => {
    const actions = await load('web-dashboard/public/account-actions.mjs');
    expect(Object.entries(actions.PROVIDER_LABELS)).toEqual(dashboard.map((id) => [id, label(id)]));
    const controller = read('web-dashboard/public/accounts-controller.mjs');
    expect(literals(controller, 'const KEY_PROVIDERS = [', ']')).toEqual(
      ids((row) => row.keyStore)
    );
    const usage = await load('web-dashboard/public/analytics-usage.mjs');
    expect(usage.PROVIDER_ORDER).toEqual([...analyticsOrder, 'other']);
    expect(Object.entries(usage.PROVIDER_LABEL)).toEqual([
      ...analyticsRows.map(([id, row]) => [id, row.analytics!.label]),
      ['other', 'Other'],
    ]);
    const source = read('web-dashboard/public/analytics-usage.mjs');
    expect(literals(source, 'const PROGRESS_TOOLS = [', ']')).toEqual(analyticsTools);
    expect([...usage.TOOLS].sort()).toEqual([...scannerKinds, 'jsonl'].sort());
    expect(Object.keys(usage.TOOL_LABEL)).toEqual(usage.TOOLS);
    for (const id of usage.TOOLS) expect(usage.TOOL_LABEL[id]).toBe(toolLabel(id));
  });

  it('accounts lists: providers by label or long label, usage-log tools in order', async () => {
    const accounts = await load('web-dashboard/public/accounts-view.mjs');
    expect(accounts.LOG_SOURCE_TOOLS).toEqual(
      Object.entries(toolTable.usageLogTools).map(([id, row]) => [
        id,
        (row as { label: string }).label,
      ])
    );
    const data = await load('web-dashboard/public/accounts-data.mjs');
    const analytics = await load('web-dashboard/public/analytics-data.mjs');
    for (const [list, expected] of [
      [data.PROVIDERS, dashboard.filter((id) => id !== 'claude' && id !== 'codex')],
      [analytics.ANALYTICS_PROVIDERS, dashboard],
    ] as Array<[Array<[string, string]>, string[]]>) {
      expect(list.map(([id]) => id).sort()).toEqual([...expected].sort());
      for (const [id, words] of list) {
        const row = providerTable.providers[id] as Row;
        expect([row.label, row.longLabel]).toContain(words);
      }
    }
  });
});

describe('the Antigravity column table owns every copy', () => {
  const columns = json('config/tables/antigravity-columns.json').columns as Array<{
    key: string;
    service: string;
    label: string;
    short: string;
    thirdParty: boolean;
    weeklyOnly: boolean;
  }>;
  const cli = columns.filter((column) => column.service === 'cli');

  it('antigravity-data.mjs labels and plan-reason subsets', () => {
    const source = read('web-dashboard/public/antigravity-data.mjs');
    expect(literals(source, 'const ANTIGRAVITY_LABELS = [', '];')).toEqual(
      columns.flatMap((column) => [column.key, column.label])
    );
    const subset = (pattern: RegExp) => {
      const match = source.match(pattern);
      expect(match).not.toBeNull();
      return [...(match?.[1] ?? '').matchAll(/'([^']*)'/g)].map((m) => m[1]).sort();
    };
    expect(
      subset(/if \(\[([^\]]*)\]\.includes\(key\) && plan\?\.thirdPartyModels === false\)/)
    ).toEqual(columns.filter((column) => column.thirdParty).map((column) => column.key).sort());
    expect(subset(/if \(\[([^\]]*)\]\.includes\(key\) && plan\?\.quotaPolicy === 'weekly'\)/)).toEqual(
      columns.filter((column) => column.weeklyOnly).map((column) => column.key).sort()
    );
  });

  it('view-model.mjs short column heads', () => {
    const source = read('web-dashboard/public/view-model.mjs');
    const from = source.indexOf('export function shortWindowLabel');
    const body = source.slice(from, source.indexOf('}[w?.key]', from));
    expect([...body.matchAll(/'([^']+)': '([^']+)'/g)].map((m) => [m[1], m[2]])).toEqual(
      columns.map((column) => [column.key, column.short])
    );
  });

  it('the Mac bar keys and captions', () => {
    const source = read('macos-bar/Sources/CCSBarCore/TrayModel.swift');
    expect(literals(source, 'public static let antigravityKeys = [', ']')).toEqual(
      cli.map((column) => column.key)
    );
    const from = source.indexOf('func antigravityLabel');
    const body = source.slice(from, source.indexOf('default: return key', from));
    expect([...body.matchAll(/case "([^"]+)": return "([^"]+)"/g)].map((m) => [m[1], m[2]])).toEqual(
      cli.map((column) => [column.key, column.short])
    );
  });

  it('the Windows bar keys and captions', () => {
    const source = read('windows-bar/CCSBar/MainWindow.xaml.cs');
    const from = source.indexOf('AntigravityKeys =');
    const body = source.slice(from, source.indexOf('];', from));
    expect([...body.matchAll(/\("([^"]+)", "([^"]+)"\)/g)].map((m) => [m[1], m[2]])).toEqual(
      cli.map((column) => [column.key, column.short])
    );
  });

  it('the zone window constants match the Rust producer', () => {
    const view = read('web-dashboard/public/view-model.mjs');
    const rust = read('crates/aac-http/src/zone_facts.rs');
    const days = view.match(/export const HOME_ZONE_DAYS = \{ past: (\d+), future: (\d+) \}/);
    expect(days?.[1]).toBe(rust.match(/const PAST_DAYS: i64 = (\d+);/)?.[1]);
    expect(days?.[2]).toBe(rust.match(/const FUTURE_DAYS: i64 = (\d+);/)?.[1]);
    const normal = (text: string | undefined) => text?.replace(/_/g, '').replace(/\s+/g, '');
    expect(normal(view.match(/const ZONE_DAY = ([\d_]+);/)?.[1])).toBe(
      normal(rust.match(/const ZONE_DAY: i64 = ([\d_]+);/)?.[1])
    );
    expect(normal(view.match(/const ZONE_STEP = ([\d_ *]+);/)?.[1])).toBe(
      normal(rust.match(/const ZONE_STEP: i64 = ([\d_ *]+);/)?.[1])
    );
  });
});
