import { describe, expect, it } from 'bun:test';
import { isSafeUsageSshAlias } from '../../../src/web-server/services/additional-usage-transport';
import {
  DASHBOARD_HOSTS,
  HOST_LABELS,
  HOST_OS,
  NAS1_SSH_ALIAS,
  helperPlatform,
} from '../../../src/web-server/services/dashboard-hosts';

describe('dashboard host facts', () => {
  it('names the four fixed computers', () => {
    expect([...DASHBOARD_HOSTS]).toEqual(['ubuntu', 'mac', 'windows', 'nas1']);
  });

  it('gives every host exactly one OS kind and one label', () => {
    expect(Object.keys(HOST_OS).sort()).toEqual([...DASHBOARD_HOSTS].sort());
    expect(Object.keys(HOST_LABELS).sort()).toEqual([...DASHBOARD_HOSTS].sort());
    expect(HOST_OS).toEqual({ ubuntu: 'linux', mac: 'mac', windows: 'windows', nas1: 'linux' });
    expect(HOST_LABELS).toEqual({
      ubuntu: 'Ubuntu',
      mac: 'Mac',
      windows: 'Windows',
      nas1: 'Nas1',
    });
  });

  it.each([
    ['ubuntu', 'ubuntu'],
    ['nas1', 'ubuntu'],
    ['mac', 'mac'],
    ['windows', 'windows'],
  ] as const)('runs the %s helpers with --platform %s', (host, platform) => {
    expect(helperPlatform(host)).toBe(platform);
  });

  it('gives every host the helper platform of its own OS', () => {
    const platformOfOs = { linux: 'ubuntu', mac: 'mac', windows: 'windows' } as const;
    for (const host of DASHBOARD_HOSTS) {
      expect(helperPlatform(host)).toBe(platformOfOs[HOST_OS[host]]);
    }
  });

  it('keeps the Nas1 ssh alias fixed and safe for the usage transport', () => {
    expect(NAS1_SSH_ALIAS).toBe('nas1-agent');
    expect(isSafeUsageSshAlias(NAS1_SSH_ALIAS)).toBe(true);
  });

  it('freezes the host tables', () => {
    expect(Object.isFrozen(HOST_OS)).toBe(true);
    expect(Object.isFrozen(HOST_LABELS)).toBe(true);
    expect(() => {
      (HOST_OS as Record<string, string>).nas1 = 'windows';
    }).toThrow();
    expect(() => {
      (HOST_LABELS as Record<string, string>).nas1 = 'Other';
    }).toThrow();
    expect(HOST_OS.nas1).toBe('linux');
    expect(HOST_LABELS.nas1).toBe('Nas1');
  });
});
