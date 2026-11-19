/**
 * L4 drift tests (DESIGN-M3-COMPLETE 2.4): config/tables/*.json is the one provider, tool and
 * host table. TS derives its lists from it (src/shared/tables.ts) and Rust generates its
 * constants from it (crates/aac-providers, whose `cargo test` binds the Rust hand lists outside
 * the workspace). Here: every TS view and every remaining hand list (the TS files fingerprinted
 * by tests/spec/wire-source-hashes.json, the Python fallback helpers and the page's JS) equals
 * its projection of the table, so a one-sided edit of either side fails. A synthetic provider
 * and tool added to a copy of the table reach every derived TS consumer.
 */
import { describe, expect, it } from 'bun:test';
import { execFileSync } from 'child_process';
import fs from 'fs';
import os from 'os';
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

describe('TS derives its lists from the tables', () => {
  it('dashboard providers, analytics order and failover kinds', async () => {
    const table = await load('src/web-server/services/dashboard-provider-table.ts');
    expect([...table.DASHBOARD_PROVIDER_IDS]).toEqual(dashboard);
    for (const id of rows.map(([id]) => id))
      expect(table.isDashboardProviderId(id)).toBe(dashboard.includes(id));
    const attribution = await load('src/web-server/services/account-analytics-attribution.ts');
    expect([...attribution.USAGE_PROVIDER_ORDER]).toEqual([...analyticsOrder, 'other']);
    const verbs = await load('src/web-server/services/agent-verbs.ts');
    const allKinds = rows.flatMap(([, row]) => row.failover.kinds);
    for (const [id, row] of rows)
      for (const kind of allKinds)
        expect(verbs.inventoryKind(id, kind)).toBe(row.failover.kinds.includes(kind));
    expect(verbs.inventoryKind('retired-provider', 'qwen-session')).toBe(false);
  });

  it('analytics tools, usage-log tools and the legacy storage hosts', async () => {
    const tools = await load('src/web-server/usage/analytics-tools.ts');
    expect([...tools.ANALYTICS_TOOLS]).toEqual(analyticsTools);
    const experiment = analyticsTools.filter((id) => toolTable.analyticsTools[id].experiment);
    expect([...tools.EXPERIMENT_ANALYTICS_TOOLS]).toEqual(experiment);
    // The type-only copy (JSON types widen `true`), bound here.
    expect([...tools.EXPERIMENT_TOOL_TYPES]).toEqual(experiment);
    const prefs = await load('src/web-server/services/dashboard-preferences.ts');
    expect([...prefs.USAGE_LOG_TOOLS]).toEqual(Object.keys(toolTable.usageLogTools));
    const hosts = await load('src/web-server/services/dashboard-hosts.ts');
    expect([...hosts.LEGACY_STORAGE_HOSTS]).toEqual(hostTable.legacyStorageHosts);
  });
});

describe('fingerprinted TS hand lists equal the table', () => {
  it('the server registry labels', async () => {
    const registry = await load('src/web-server/services/dashboard-provider-registry.ts');
    const entries = registry.buildDashboardProviders([], [], []) as Array<{
      id: string;
      label: string;
      longLabel: string;
    }>;
    expect(entries.map((e) => [e.id, e.label, e.longLabel])).toEqual(
      dashboard.map((id) => [id, label(id), providerTable.providers[id].longLabel])
    );
  });

  it("Analytics' provider table and the dashboard projection's additional providers", async () => {
    const projection = await load('src/web-server/services/account-dashboard-projection.ts');
    const expected = analyticsRows
      .filter(([id]) => id !== 'claude' && id !== 'codex')
      .map(([id, row]) => [id, row.analytics!.label]);
    expect(projection.ADDITIONAL_PROVIDERS).toEqual(expected);
    const analytics = await load('src/web-server/services/account-analytics-providers.ts');
    expect(analytics.defaultAnalyticsProviderTable()).toEqual(
      analyticsRows.map(([id, row]) => ({
        id,
        label: row.analytics!.label,
        order: row.analytics!.order,
      }))
    );
  });

  it("the remote scan's kinds", async () => {
    const transport = await load('src/web-server/services/analytics-remote-transport.ts');
    expect([...transport.PYTHON_KINDS]).toEqual(analyticsTools);
    // The transport's own runtime kind list (module-private, it validates requests, rows and
    // discovery) is the table's scanner kinds exactly: discovery keeps a kind only when the
    // transport accepts it (L4 review L1).
    const entry = {
      installed: false,
      signals: [],
      roots: [],
      counts: { roots: 0, filesFound: 0, filesRead: 0, filesFailed: 0, newestMtime: null },
    };
    const offered = [...scannerKinds, 'zz-not-a-kind', 'toString'];
    const discovery = transport.parseAnalyticsDiscovery({
      kinds: Object.fromEntries(offered.map((kind) => [kind, entry])),
    });
    expect(Object.keys(discovery.kinds)).toEqual(scannerKinds);
    const sources = await load('src/web-server/services/analytics-remote-sources.ts');
    expect(sources.analyticsRemoteTargets('nas1')).toEqual(scannerKinds);
  });
});

describe('the Python fallback helpers equal the table', () => {
  const python = (code: string) =>
    JSON.parse(
      execFileSync(
        'python3',
        [
          // -I ignores PYTHON* variables, so -B keeps bytecode out of scripts/.
          '-I',
          '-B',
          '-c',
          `import sys,json;sys.path.insert(0,${JSON.stringify(path.join(root, 'scripts/account-usage'))});${code}`,
        ],
        { encoding: 'utf8' }
      )
    );

  it('plan_common providers, key-store providers and Retry-After policy', () => {
    const got = python(
      'import plan_common as p;r=p.retry_after_seconds;' +
        'print(json.dumps([p.PROVIDERS,list(p.KEY_PROVIDERS),r(None),r("1"),r("99999999"),r("120")]))'
    );
    const retry = providerTable.planRetryAfter;
    expect(got).toEqual([
      Object.fromEntries(ids((row) => row.quota).map((id) => [id, label(id)])),
      ids((row) => row.keyStore),
      retry.defaultS,
      retry.floorS,
      retry.ceilingS,
      120,
    ]);
  });

  it('failover kinds, labels and the additional helper providers', () => {
    const got = python(
      'import failover_usage as u,failover_additional as a;' +
        'print(json.dumps([{k:sorted(v) for k,v in u.KINDS.items()},u.LABELS,sorted(a.PROVIDERS)]))'
    );
    expect(got[0]).toEqual(
      Object.fromEntries(rows.map(([id, row]) => [id, [...row.failover.kinds].sort()]))
    );
    expect(got[1]).toEqual(Object.fromEntries(rows.map(([id, row]) => [id, row.failover.label])));
    expect(got[2]).toEqual(ids((row) => row.card).sort());
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

describe('a synthetic provider and tool added to the table', () => {
  it('reach every derived TS consumer', async () => {
    const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-l4-synthetic-'));
    try {
      // config/private-files.json: the L2 registry the TS writers load beside the tables.
      for (const folder of [
        'src',
        'config/tables',
        'config/private-files.json',
        'config/agent-artifacts.json',
        'config/rust-service-routes.json',
        'config/secret-policy.json',
        'web-dashboard/public',
      ])
        fs.cpSync(path.join(root, folder), path.join(fixture, folder), { recursive: true });
      fs.symlinkSync(path.join(root, 'node_modules'), path.join(fixture, 'node_modules'), 'dir');
      const edit = (name: string, change: (table: Record<string, any>) => void) => {
        const file = path.join(fixture, 'config/tables', name);
        const table = JSON.parse(fs.readFileSync(file, 'utf8'));
        change(table);
        fs.writeFileSync(file, JSON.stringify(table));
      };
      edit('providers.json', (table) => {
        table.providers['zz-synthetic'] = {
          ...table.providers['opencode-go'],
          label: 'Synthetic',
          longLabel: 'Synthetic provider',
          analytics: { order: analyticsOrder.length, label: 'Synthetic' },
          failover: { label: 'Synthetic', kinds: ['zz-synthetic-credential'] },
        };
      });
      edit('tools.json', (table) => {
        table.analyticsTools['zz-tool'] = {
          label: 'Synthetic tool',
          experiment: true,
          extraRoots: true,
        };
        table.usageLogTools['zz-log'] = { label: 'Synthetic log' };
      });
      const providerTableModule = await load(
        'src/web-server/services/dashboard-provider-table.ts',
        fixture
      );
      expect([...providerTableModule.DASHBOARD_PROVIDER_IDS]).toEqual([
        ...dashboard,
        'zz-synthetic',
      ]);
      expect(providerTableModule.isDashboardProviderId('zz-synthetic')).toBe(true);
      const attribution = await load(
        'src/web-server/services/account-analytics-attribution.ts',
        fixture
      );
      expect([...attribution.USAGE_PROVIDER_ORDER]).toEqual([
        ...analyticsOrder,
        'zz-synthetic',
        'other',
      ]);
      const verbs = await load('src/web-server/services/agent-verbs.ts', fixture);
      expect(verbs.inventoryKind('zz-synthetic', 'zz-synthetic-credential')).toBe(true);
      expect(verbs.inventoryKind('zz-synthetic', 'opencode-go-credential')).toBe(false);
      const tools = await load('src/web-server/usage/analytics-tools.ts', fixture);
      expect(tools.ANALYTICS_TOOLS).toContain('zz-tool');
      expect(tools.EXPERIMENT_ANALYTICS_TOOLS).toContain('zz-tool');
      expect(tools.isAnalyticsTool('zz-tool')).toBe(true);
      const prefs = await load('src/web-server/services/dashboard-preferences.ts', fixture);
      expect(prefs.USAGE_LOG_TOOLS).toContain('zz-log');
      expect(
        prefs.isDashboardPreferences({
          ...prefs.defaultDashboardPreferences(),
          usageLogSources: [
            { id: 'fixture', host: 'ubuntu', tool: 'zz-log', path: '/var/log/fixture' },
          ],
        })
      ).toBe(true);
    } finally {
      fs.rmSync(fixture, { recursive: true, force: true });
    }
  });
});
