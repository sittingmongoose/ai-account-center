import { describe, expect, it } from 'bun:test';
import {
  parseSelfCgroup,
  scanCgroupForForeignProcesses,
  startCgroupForeignCheck,
} from '../../../src/web-server/services/cgroup-foreign-check';

const SELF = '0::/user.slice/user-1000.slice/user@1000.service/app.slice/ccs-dashboard.service';

function fixtureDeps(extra: Record<string, string | null> = {}) {
  const files: Record<string, string | null> = {
    '/proc/100/cgroup': SELF,
    '/proc/200/cgroup': SELF,
    '/proc/200/comm': 'codex\n',
    '/proc/201/cgroup': SELF,
    '/proc/201/comm': 'python3\n',
    '/proc/202/cgroup': '0::/user.slice/user-1000.slice/user@1000.service/app.slice/other.service',
    '/proc/202/comm': 'codex\n',
    '/proc/203/cgroup': null, // Exited mid-scan.
    '/proc/204/cgroup': `${SELF}/nested`,
    '/proc/204/comm': 'ChatGPT\n',
    ...extra,
  };
  return {
    platform: 'linux' as const,
    selfPid: 100,
    readTextFile: (path: string) => (path in files ? files[path] : null),
    listDir: (path: string) =>
      path === '/proc' ? ['1', '100', '200', '201', '202', '203', '204', 'self', 'sys'] : [],
  };
}

describe('cgroup-foreign-check', () => {
  it('parses v2 and v1 cgroup paths', () => {
    expect(parseSelfCgroup('0::/user.slice/app.slice/x.service\n')).toBe(
      '/user.slice/app.slice/x.service'
    );
    expect(parseSelfCgroup('11:memory:/user.slice/x\n2:systemd:/user.slice/y.service\n')).toBe(
      '/user.slice/y.service'
    );
    expect(parseSelfCgroup('11:memory:/user.slice/x\n')).toBe(null);
    expect(parseSelfCgroup('')).toBe(null);
  });

  it('reports only foreign processes in the same cgroup tree', () => {
    const result = scanCgroupForForeignProcesses(fixtureDeps());
    expect(result?.selfCgroup).toBe(
      '/user.slice/user-1000.slice/user@1000.service/app.slice/ccs-dashboard.service'
    );
    expect(result?.apps).toEqual([
      { pid: 200, name: 'codex' },
      { pid: 204, name: 'ChatGPT' },
    ]);
    expect(result?.others).toEqual([{ pid: 201, name: 'python3' }]);
  });

  it('returns null off linux or without cgroup info', () => {
    expect(scanCgroupForForeignProcesses({ ...fixtureDeps(), platform: 'darwin' })).toBe(null);
    expect(scanCgroupForForeignProcesses({ ...fixtureDeps(), readTextFile: () => null })).toBe(
      null
    );
  });

  it('warns once about caged apps and stops cleanly', () => {
    const warnings: string[] = [];
    const infos: string[] = [];
    const stop = startCgroupForeignCheck({
      ...fixtureDeps(),
      intervalMs: 60 * 60 * 1000,
      warn: (_event, message) => {
        warnings.push(message);
      },
      info: (_event, message) => {
        infos.push(message);
      },
    });
    stop();
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain('codex (pid 200)');
    expect(warnings[0]).toContain('KillMode=control-group');
    expect(infos).toHaveLength(0);
  });

  it('stays info-level when only unknown processes are caged', () => {
    const deps = fixtureDeps({
      '/proc/200/comm': 'bash\n',
      '/proc/204/cgroup': '0::/elsewhere',
    });
    const warnings: string[] = [];
    const infos: string[] = [];
    const stop = startCgroupForeignCheck({
      ...deps,
      intervalMs: 60 * 60 * 1000,
      warn: (_event, message) => {
        warnings.push(message);
      },
      info: (_event, message) => {
        infos.push(message);
      },
    });
    stop();
    expect(warnings).toHaveLength(0);
    expect(infos).toHaveLength(1);
  });

  it('never throws from a failed scan', () => {
    const stop = startCgroupForeignCheck({
      platform: 'linux',
      readTextFile: () => {
        throw new Error('fixture io');
      },
      listDir: () => {
        throw new Error('fixture io');
      },
      intervalMs: 60 * 60 * 1000,
    });
    stop();
  });
});
