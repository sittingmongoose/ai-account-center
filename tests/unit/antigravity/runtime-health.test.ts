/**
 * The read-only runtime health check over invented bundles and recorded
 * argv: no service is started, stopped or queried except the packaged probe
 * script run against a disposable temporary bundle.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import net from 'net';
import os from 'os';
import path from 'path';
import {
  ANTIGRAVITY_RUNTIME_UNIT,
  createRuntimeServiceProblemReader,
  defaultRuntimeHealthScript,
  describeRuntimeServiceProblem,
  diagnoseRuntimeService,
  diagnoseRuntimeServiceAsync,
  parserProblemFromProbe,
  publicRuntimeServiceProblem,
  unitProblemFromShow,
} from '../../../src/antigravity/runtime-health';
import type { AntigravityRuntimeServiceProblem } from '../../../src/antigravity/usage-contract';

const MISSING_PYTE: AntigravityRuntimeServiceProblem = {
  reason: 'missing-python-module',
  module: 'pyte',
  python: '3.14',
  builtFor: '3.13',
  exitStatus: null,
};
const PROBE_MISSING = JSON.stringify({
  ok: false,
  reason: 'missing-python-module',
  module: 'pyte',
  python: '3.14',
  builtFor: '3.13',
});
const FAILED_UNIT = 'ActiveState=failed\nResult=exit-code\nExecMainStatus=1\n';
let root: string;

beforeEach(() => {
  root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-health-')));
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

describe('Antigravity runtime health', () => {
  it('reads a probe line and a failed unit, and nothing else', () => {
    expect(parserProblemFromProbe(PROBE_MISSING)).toEqual(MISSING_PYTE);
    expect(parserProblemFromProbe('{"ok":true,"python":"3.14"}')).toBeNull();
    expect(parserProblemFromProbe('{"ok":false,"reason":"probe-failed"}')).toBeNull();
    expect(parserProblemFromProbe('not json')).toBeNull();
    expect(parserProblemFromProbe(null)).toBeNull();
    expect(
      parserProblemFromProbe(
        JSON.stringify({ ok: false, reason: 'parser-mismatch', module: 'x y', python: 3 })
      )
    ).toEqual({
      reason: 'parser-mismatch',
      module: null,
      python: null,
      builtFor: null,
      exitStatus: null,
    });
    expect(unitProblemFromShow(FAILED_UNIT)).toEqual({
      reason: 'service-failed',
      module: null,
      python: null,
      builtFor: null,
      exitStatus: 1,
    });
    expect(
      unitProblemFromShow('ActiveState=inactive\nResult=success\nExecMainStatus=0\n')
    ).toBeNull();
    expect(unitProblemFromShow('ActiveState=failed\nExecMainStatus=nope\n')?.exitStatus).toBeNull();
    expect(unitProblemFromShow(null)).toBeNull();
  });

  it('says it plainly', () => {
    expect(describeRuntimeServiceProblem(MISSING_PYTE)).toBe(
      'Runtime service failed: missing Python module pyte (system Python is 3.14; the runtime bundle was built for 3.13)'
    );
    expect(describeRuntimeServiceProblem({ ...MISSING_PYTE, builtFor: '3.14' })).toBe(
      'Runtime service failed: missing Python module pyte'
    );
    expect(
      describeRuntimeServiceProblem({
        reason: 'service-failed',
        module: null,
        python: null,
        builtFor: null,
        exitStatus: 1,
      })
    ).toBe('Runtime service failed: exit status 1');
  });

  it('scrubs anything unexpected', () => {
    expect(publicRuntimeServiceProblem({ ...MISSING_PYTE, extra: 'private' })).toEqual(
      MISSING_PYTE
    );
    expect(publicRuntimeServiceProblem({ reason: 'not-running' })).toBeNull();
    expect(publicRuntimeServiceProblem(null)).toBeNull();
    expect(publicRuntimeServiceProblem([MISSING_PYTE])).toBeNull();
    expect(
      publicRuntimeServiceProblem({ reason: 'service-failed', exitStatus: 256, module: 'a b' })
    ).toEqual({
      reason: 'service-failed',
      module: null,
      python: null,
      builtFor: null,
      exitStatus: null,
    });
  });

  it('asks the service interpreter first and the unit only when the parser is fine', async () => {
    const calls: Array<[string, string[]]> = [];
    const bundle = path.join(root, 'b'.repeat(64));
    const answers =
      (parser: string | null, unit: string | null) => (file: string, args: string[]) => {
        calls.push([file, args]);
        return file === 'systemctl' ? unit : parser;
      };
    expect(diagnoseRuntimeService(bundle, answers(PROBE_MISSING, FAILED_UNIT))).toEqual(
      MISSING_PYTE
    );
    expect(calls).toEqual([
      ['/usr/bin/python3', ['-I', '-B', defaultRuntimeHealthScript(), bundle]],
    ]);
    calls.length = 0;
    expect(diagnoseRuntimeService(bundle, answers('{"ok":true}', FAILED_UNIT))?.reason).toBe(
      'service-failed'
    );
    expect(calls[1]).toEqual([
      'systemctl',
      ['--user', 'show', ANTIGRAVITY_RUNTIME_UNIT, '--property=ActiveState,Result,ExecMainStatus'],
    ]);
    expect(diagnoseRuntimeService(bundle, answers(null, null))).toBeNull();
    const sync = answers(PROBE_MISSING, null);
    expect(
      await diagnoseRuntimeServiceAsync(bundle, async (file, args) => sync(file, args))
    ).toEqual(MISSING_PYTE);
    expect(fs.existsSync(defaultRuntimeHealthScript())).toBe(true);
  });

  it.skipIf(!fs.existsSync('/usr/bin/python3'))(
    'names the missing module of a real legacy bundle after a Python upgrade',
    () => {
      const bundle = path.join(root, 'c'.repeat(64));
      const site = path.join(bundle, 'venv', 'lib', 'python3.0', 'site-packages', 'pyte');
      fs.mkdirSync(site, { recursive: true });
      fs.writeFileSync(path.join(site, '__init__.py'), '# invented fixture package\n');
      fs.writeFileSync(
        path.join(bundle, 'venv', 'pyvenv.cfg'),
        'home = /usr/bin\nversion = 3.0.1\n'
      );
      const before = fs.readdirSync(bundle, { recursive: true }).sort();
      let unitAsked = false;
      const problem = diagnoseRuntimeService(bundle, (file, args, timeoutMs) => {
        if (file === 'systemctl') {
          unitAsked = true;
          return null;
        }
        const result = Bun.spawnSync([file, ...args], { timeout: timeoutMs });
        return result.exitCode === 0 ? result.stdout.toString() : null;
      });
      expect(problem?.reason).toBe('missing-python-module');
      expect(problem?.module).toBe('pyte');
      expect(problem?.builtFor).toBe('3.0');
      expect(problem?.python).toMatch(/^3\.\d+$/);
      expect(unitAsked).toBe(false);
      expect(fs.readdirSync(bundle, { recursive: true }).sort()).toEqual(before);
    }
  );

  it('probes nothing while the socket exists and at most once per interval otherwise', async () => {
    const socketPath = path.join(root, 'control.sock');
    let now = 1_000;
    let probes = 0;
    const read = createRuntimeServiceProblemReader(
      { bundleDirectory: path.join(root, 'd'.repeat(64)), socketPath },
      {
        now: () => now,
        ttlMs: 60_000,
        diagnose: async () => {
          probes++;
          return { ...MISSING_PYTE, privatePath: 'hidden' } as AntigravityRuntimeServiceProblem;
        },
      }
    );
    expect(await read()).toEqual(MISSING_PYTE);
    expect(await read()).toEqual(MISSING_PYTE);
    expect(probes).toBe(1);
    now += 60_001;
    await read();
    expect(probes).toBe(2);
    const server = net.createServer();
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    try {
      expect(await read()).toBeNull();
      expect(probes).toBe(2);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    const failing = createRuntimeServiceProblemReader(
      { bundleDirectory: root, socketPath: path.join(root, 'missing.sock') },
      {
        diagnose: async () => {
          throw new Error('synthetic probe failure');
        },
      }
    );
    expect(await failing()).toBeNull();
  });
});
