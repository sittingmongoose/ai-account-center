/** A sixth computer is only one table entry; exercise real consumers in an isolated source fixture. */
import { it, expect } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { pathToFileURL } from 'url';
import { execFileSync } from 'child_process';

it('every per-computer consumer picks up a hypothetical extra Linux entry', async () => {
  const root = path.resolve(import.meta.dir, '../..');
  const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-sixth-computer-'));
  try {
    for (const folder of [
      'src',
      'web-dashboard/public',
      'scripts/agent-usage',
      'docs/agent-reports/schemas',
      'native/aac-status/src',
      'web-dashboard/leptos',
    ]) {
      fs.cpSync(path.join(root, folder), path.join(fixture, folder), { recursive: true });
    }
    fs.mkdirSync(path.join(fixture, 'scripts'), { recursive: true });
    fs.copyFileSync(
      path.join(root, 'scripts/build-status-native.js'),
      path.join(fixture, 'scripts/build-status-native.js')
    );
    fs.copyFileSync(
      path.join(root, 'scripts/build-ui.js'),
      path.join(fixture, 'scripts/build-ui.js')
    );
    fs.copyFileSync(
      path.join(root, 'scripts/ui-build-lock.js'),
      path.join(fixture, 'scripts/ui-build-lock.js')
    );
    for (const name of ['Cargo.toml', 'Cargo.lock', 'build.rs', 'rust-toolchain.toml'])
      fs.copyFileSync(
        path.join(root, 'native/aac-status', name),
        path.join(fixture, 'native/aac-status', name)
      );
    fs.symlinkSync(path.join(root, 'node_modules'), path.join(fixture, 'node_modules'), 'dir');
    const file = path.join(fixture, 'web-dashboard/public/computers.json');
    const rows = JSON.parse(fs.readFileSync(file, 'utf8'));
    rows.future = {
      ...rows.nas1,
      label: 'Future Linux',
      machineId: 'future',
      machineLabel: 'Future Linux',
      shortLabel: 'Future Linux',
      sentenceLabel: 'Future Linux',
      sshAlias: 'future-linux',
      names: ['future-linux'],
      legacy: false,
    };
    fs.writeFileSync(file, JSON.stringify(rows));
    const load = (module: string) => import(pathToFileURL(path.join(fixture, module)).href);
    const hosts = await load('src/web-server/services/dashboard-hosts.ts');
    expect(hosts.DASHBOARD_HOSTS).toHaveLength(6);
    expect(hosts.helperPlatform('future')).toBe('ubuntu');
    expect(hosts.sourceComputer({ platform: 'ubuntu', sshHost: 'future-linux' })).toBe('future');
    const buildStatus = await load('scripts/build-status-native.js');
    const buildUi = await load('scripts/build-ui.js');
    const leptosCrate = path.join(fixture, 'web-dashboard/leptos');
    const beforeLeptos = buildUi.leptosFingerprint(leptosCrate).sha256;
    const beforeHash = buildStatus.sourceHash();
    fs.writeFileSync(file, fs.readFileSync(file, 'utf8') + '\n');
    expect(buildStatus.sourceHash()).not.toBe(beforeHash);
    expect(buildUi.leptosFingerprint(leptosCrate).sha256).not.toBe(beforeLeptos);
    const activity = await load('src/web-server/services/account-analytics-activity.ts');
    expect(
      activity
        .coldScanningSourceStates()
        .filter((entry: { host: string }) => entry.host === 'future')
    ).toHaveLength(7);
    const updates = await load('src/web-server/services/app-update-contract.ts');
    expect(updates.PLATFORMS).toContain('future');
    expect(updates.EXPECTED_RESULTS).toBe(60);
    const transport = await load('src/web-server/services/app-update-hosts.ts');
    expect(transport.appUpdateInvocation('future', '').args.slice(-2)).toEqual([
      'future-linux',
      'AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform ubuntu',
    ]);
    const remote = await load('src/web-server/services/analytics-remote-transport.ts');
    expect(remote.REMOTE_ANALYTICS_HOSTS).toContain('future');
    expect((await remote.resolveAnalyticsRemoteHosts()).future).toBe('future-linux');
    const analytics = await load('src/web-server/services/analytics-remote-sources.ts');
    expect(analytics.analyticsRemoteTargets('future')).toHaveLength(6);
    const scanned: string[] = [];
    const config = await load('src/utils/config-manager.ts');
    await config.runWithScopedCcsHome(fixture, () =>
      analytics.loadAnalyticsRemoteSources(0, {
        cacheDir: path.join(fixture, 'cache'),
        runHelper: async (_alias: string, host: string) => {
          scanned.push(host);
          return { version: 2, truncated: false, kinds: {}, rows: [], srows: [] };
        },
      })
    );
    expect(scanned).toContain('future');
    const usage = await load('src/web-server/services/usage-failover-transport.ts');
    expect(await usage.configuredUsageHosts()).toContainEqual({
      host: 'future',
      platform: 'ubuntu',
      sshHost: 'future-linux',
    });
    const sources = await load('src/web-server/services/additional-account-service.ts');
    expect(sources.sourceHost({ platform: 'ubuntu', sshHost: 'future-linux' })).toBe('future');
    const prefs = await load('src/web-server/services/dashboard-preferences.ts');
    expect(
      prefs.isDashboardPreferences({
        ...prefs.defaultDashboardPreferences(),
        usageLogSources: [
          { id: 'fixture', host: 'future', tool: 'codex', path: '/var/log/fixture' },
        ],
      })
    ).toBe(true);
    const peers = await load('src/web-server/services/muse-usage-peer-bindings.ts');
    expect(
      peers.parseMuseUsagePeer({
        host: 'future',
        identity: { kind: 'muse-native-credential', key: 'a'.repeat(64) },
      })?.host
    ).toBe('future');
    const status = await load('src/web-server/status/schema-helpers.ts');
    expect(status.MACHINES).toContain('future');
    expect(status.isWriterName('future.fixture')).toBe(true);
    const collectors = await load('src/web-server/status/status-collectors.ts');
    expect(collectors.REMOTE_MACHINES.future).toBe('linux');
    expect(collectors.machineSshAlias('future')).toBe('future-linux');
    expect(collectors.registryMachine('future-linux', null)).toBe('future');
    const pins = await load('src/web-server/status/pinned-threads.ts');
    expect(pins.PINS_MACHINES).toContain('future');
    const reports = await load('src/web-server/status/agent-reports-reader.ts');
    const schema = JSON.parse(
      reports
        .computerReportSchema(
          fs.readFileSync(path.join(fixture, 'docs/agent-reports/schemas/thread.json'))
        )
        .toString()
    );
    expect(new RegExp(schema.properties.writer.pattern).test('future.fixture')).toBe(true);
    expect(new RegExp(schema.properties.writer.pattern).test('futureXfixture')).toBe(false);
    expect(new RegExp(schema.properties.writer.pattern).test('unregistered.fixture')).toBe(false);
    expect(schema.properties.agent.properties.machine.enum).toContain('future');
    const view = await load('web-dashboard/public/view-model.mjs');
    expect(view.UPDATE_HOST_IDS).toContain('future');
    expect(view.updateTotal(null)).toBe(60);
    expect(view.platformLabel('future')).toBe('Future Linux');
    const accounts = await load('web-dashboard/public/accounts-view.mjs');
    for (const tool of ['omp', 'muse', 'zcode', 'claude-code', 'codex'])
      expect(accounts.LOG_SOURCE_HOSTS[tool]).toContain('future');
    expect(accounts.LOG_SOURCE_HOSTS.jsonl).toEqual(['ubuntu']);
    const reader = path.join(fixture, 'scripts/agent-usage/aac-usage');
    const labels = JSON.parse(
      execFileSync(
        'python3',
        [
          '-c',
          'import runpy,json,sys;print(json.dumps(runpy.run_path(sys.argv[1])["HOSTS"]))',
          reader,
        ],
        { encoding: 'utf8', env: { ...process.env, CCS_HOME: fixture } }
      )
    );
    expect(labels.future).toBe('Future Linux');
    // Both separately compiled Rust consumers embed this same table, never parallel allowlists.
    for (const source of [
      'native/aac-status/src/computers.rs',
      'web-dashboard/leptos/src/status.rs',
    ])
      expect(fs.readFileSync(path.join(root, source), 'utf8')).toContain('public/computers.json');
  } finally {
    fs.rmSync(fixture, { recursive: true, force: true });
  }
}, 30_000);
