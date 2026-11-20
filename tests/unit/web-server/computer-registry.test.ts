import { describe, it, expect } from 'bun:test';
import {
  COMPUTERS,
  DASHBOARD_HOSTS,
  HOST_OS,
  SSH_ALIASES,
  helperPlatform,
  sourceComputer,
  localComputer,
} from '../../../src/web-server/services/dashboard-hosts';
import {
  appUpdateInvocation,
  helperHashQuery,
} from '../../../src/web-server/services/app-update-hosts';
import { PLATFORMS, EXPECTED_RESULTS } from '../../../src/web-server/services/app-update-contract';
import { configuredUsageHosts } from '../../../src/web-server/services/usage-failover-transport';
import { sourceHost } from '../../../src/web-server/services/additional-account-service';
import {
  isDashboardPreferences,
  defaultDashboardPreferences,
} from '../../../src/web-server/services/dashboard-preferences';
import { isWriterName } from '../../../src/web-server/status/schema-helpers';
import {
  registryMachine,
  machineSshAlias,
  REMOTE_MACHINES,
} from '../../../src/web-server/status/status-collectors';
import { analyticsRemoteTargets } from '../../../src/web-server/services/analytics-remote-sources';
import { resolveAnalyticsRemoteHosts } from '../../../src/web-server/services/analytics-remote-transport';
import { parseSourceManifest } from '../../../src/web-server/services/account-usage-manifest';
import { parseAccountRegistry } from '../../../src/web-server/services/account-registry-v2';
import { parseMuseUsagePeer } from '../../../src/web-server/services/muse-usage-peer-bindings';

const isUsageLogSource = (source: unknown) =>
  isDashboardPreferences({ ...defaultDashboardPreferences(), usageLogSources: [source] });

describe('package-owned computer registry', () => {
  it('adds Alexis as another Windows computer while preserving OS kinds', () => {
    expect(DASHBOARD_HOSTS).toEqual(['ubuntu', 'mac', 'windows', 'nas1', 'alexis', 'odroid']);
    expect(HOST_OS.alexis).toBe('windows');
    expect(helperPlatform('alexis')).toBe('windows');
    expect(helperPlatform('nas1')).toBe('ubuntu');
    expect(COMPUTERS.alexis.label).toBe('Alexis PC');
    expect(PLATFORMS).toEqual(DASHBOARD_HOSTS);
    expect(EXPECTED_RESULTS).toBe(72);
    expect(Object.isFrozen(COMPUTERS.alexis)).toBe(true);
  });
  it('uses only the fixed Alexis SSH alias and a Windows helper command', async () => {
    const invocation = await appUpdateInvocation('alexis', '');
    expect(invocation.args.at(-2)).toBe('alexis-windows');
    const script = Buffer.from(invocation.args.at(-1)!.split(' ').at(-1)!, 'base64').toString(
      'utf16le'
    );
    expect(script).toContain('& python.exe $helper --apply --platform windows');
    expect(script).not.toContain('--dashboard-job');
    expect(helperHashQuery('alexis')).toBe(helperHashQuery('windows'));
    expect(SSH_ALIASES.alexis).toBe('alexis-windows');
  });
  it('includes Alexis in peer discovery and attributes its quota separately', async () => {
    expect(await configuredUsageHosts()).toContainEqual({
      host: 'alexis',
      platform: 'windows',
      sshHost: 'alexis-windows',
    });
    expect(sourceHost({ platform: 'windows', sshHost: 'alexis-windows' })).toBe('alexis');
    expect(sourceHost({ platform: 'windows', sshHost: SSH_ALIASES.windows })).toBe('windows');
    expect(
      parseMuseUsagePeer({
        host: 'alexis',
        identity: { kind: 'muse-native-credential', key: 'a'.repeat(64) },
      })?.host
    ).toBe('alexis');
  });
  it('maps agent-report and known Alexis hostnames before the generic PC fallback', () => {
    for (const name of ['alexis', 'alexis-windows', 'AlexisGamingPC'])
      expect(registryMachine(name, null)).toBe('alexis');
    expect(localComputer('AlexisGamingPC', 'win32')).toBe('alexis');
    expect(isWriterName('alexis.fixture')).toBe(true);
    expect(isWriterName('unregistered.fixture')).toBe(false);
    expect(machineSshAlias('alexis')).toBe('alexis-windows');
    expect(REMOTE_MACHINES.alexis).toBe('windows');
  });
  it('accepts Windows log paths on either Windows computer', () => {
    for (const host of ['windows', 'alexis'])
      expect(isUsageLogSource({ id: 'fixture', host, tool: 'codex', path: 'C:\\logs' })).toBe(true);
    expect(isUsageLogSource({ id: 'fixture', host: 'nas1', tool: 'codex', path: 'C:\\logs' })).toBe(
      false
    );
  });
  it('treats Odroid as another remote Ubuntu computer wherever Nas1 is one', async () => {
    expect(SSH_ALIASES.odroid).toBe('odroid');
    expect(HOST_OS.odroid).toBe(HOST_OS.nas1);
    expect(helperPlatform('odroid')).toBe('ubuntu');
    expect(REMOTE_MACHINES.odroid).toBe('linux');
    expect(machineSshAlias('odroid')).toBe('odroid');
    for (const name of ['odroid', 'Odroid', 'odroid.local'])
      expect(registryMachine(name, null)).toBe('odroid');
    expect(localComputer('Odroid', 'linux')).toBe('odroid');
    expect(isWriterName('odroid.fixture')).toBe(true);
    const invocation = await appUpdateInvocation('odroid', '');
    expect(invocation.args.at(-2)).toBe('odroid');
    expect(invocation.args.at(-1)).toContain('--platform ubuntu');
    expect(await configuredUsageHosts()).toContainEqual({
      host: 'odroid',
      platform: 'ubuntu',
      sshHost: 'odroid',
    });
    expect(sourceHost({ platform: 'ubuntu', sshHost: 'odroid' })).toBe('odroid');
    expect(analyticsRemoteTargets('odroid')).toHaveLength(analyticsRemoteTargets('nas1').length);
    expect((await resolveAnalyticsRemoteHosts()).odroid).toBe('odroid');
    // Dynamic: view-model.mjs imports computers.json as an ES module, and a static import here would
    // replace the CommonJS require dashboard-hosts.ts reads the same file with (a Bun module-cache key).
    const view = await import('../../../web-dashboard/public/view-model.mjs');
    expect(view.UPDATE_HOST_IDS).toEqual(DASHBOARD_HOSTS);
    expect(view.platformLabel('odroid')).toBe('Odroid');
  });
  it('keeps both generations of account files on the existing platform + alias protocol', () => {
    for (const [platform, sshHost, host] of [
      ['windows', 'alexis-windows', 'alexis'],
      ['ubuntu', 'nas1-agent', 'nas1'],
      ['windows', SSH_ALIASES.windows, 'windows'],
    ] as const) {
      const manifest = parseSourceManifest(
        JSON.stringify({ version: 1, sources: [{ provider: 'cursor', platform, sshHost }] })
      );
      expect(manifest.valid).toBe(true);
      expect(sourceComputer(manifest.sources.find((s) => s.provider === 'cursor')!)).toBe(host);
      const registry = parseAccountRegistry({
        version: 2,
        accounts: [
          {
            id: 'qwen:acct:0a1b2c3d',
            provider: 'qwen',
            platform,
            sshHost,
            credential: { kind: 'browser-capsule', capsuleId: '0a1b2c3d' },
          },
        ],
      });
      expect(registry).not.toBeNull();
      expect(sourceComputer(registry!.accounts[0])).toBe(host);
      // Original validators still accept these exact on-disk shapes: no new platform or host field.
      expect(Object.keys(registry!.accounts[0])).not.toContain('host');
      expect(['ubuntu', 'mac', 'windows']).toContain(registry!.accounts[0].platform);
    }
    expect(
      parseSourceManifest(
        JSON.stringify({ version: 1, sources: [{ provider: 'cursor', platform: 'windows' }] })
      ).valid
    ).toBe(true);
  });
});
