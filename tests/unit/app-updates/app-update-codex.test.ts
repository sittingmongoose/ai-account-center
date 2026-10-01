import { beforeEach, describe, expect, it } from 'bun:test';
const bridge = require('../../../scripts/app-updates/app_update_codex.cjs');
beforeEach(() =>
  Object.assign(bridge.output, {
    appId: 'codex-cli',
    platform: 'ubuntu',
    status: 'failed',
    previousVersion: null,
    version: null,
    manager: 'native',
    messageCode: 'update_failed',
    updateAttempted: false,
    restartedProcesses: 0,
  })
);
function fixture() {
  let clock = 0;
  let version = '1.0.0';
  let pending = false;
  const events: string[] = [];
  let busy = 2;
  return {
    events,
    deps: {
      now: () => clock,
      sleep: async (ms: number) => {
        clock += ms;
        events.push('wait');
      },
      readVersion: async () => version,
      update: async () => {
        version = '2.0.0';
        events.push('update');
      },
      pending: () => pending,
      mark: () => {
        pending = true;
        events.push('mark');
      },
      clear: () => {
        pending = false;
        events.push('clear');
      },
      restartProxies: async () => {
        events.push('proxy-reconnect');
      },
      hasOldProxies: async () => false,
      runtime: () => ({
        stop: async () => {
          events.push('stop');
          if (busy-- > 0) throw Object.assign(new Error('private'), { code: 'busy' });
        },
        start: async () => {
          events.push('start');
        },
        dispose: async () => {
          events.push('dispose');
        },
      }),
    },
    pending: () => pending,
  };
}
describe('Codex update idle and restart coordinator', () => {
  it('queues busy work, then reconnects proxies under stop/start protection', async () => {
    const value = fixture();
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('updated');
    expect(value.events).toEqual([
      'update',
      'mark',
      'stop',
      'wait',
      'stop',
      'wait',
      'stop',
      'proxy-reconnect',
      'start',
      'clear',
      'dispose',
    ]);
    expect(value.pending()).toBe(false);
  });
  it('retains pending restart after a failure so a later explicit click retries', async () => {
    const value = fixture();
    value.deps.runtime = () => ({
      stop: async () => {},
      start: async () => {
        throw new Error('PRIVATE_SENTINEL');
      },
      dispose: async () => {},
    });
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
  });
  it('does not stop a current package without pending restart', async () => {
    const value = fixture();
    value.deps.update = async () => {
      value.events.push('update');
    };
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('current');
    expect(value.events).toEqual(['update']);
  });
  it('reports an unreplaced proxy as a restart failure', async () => {
    const value = fixture();
    value.deps.hasOldProxies = async () => true;
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
  });
});
