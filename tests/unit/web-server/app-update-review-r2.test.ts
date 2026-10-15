import { afterEach, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AppUpdateBusyError,
  AppUpdateService,
} from '../../../src/web-server/services/app-update-service';
import {
  HelperExecutionError,
  runHelperProcess,
  updaterEnvironment,
} from '../../../src/web-server/services/app-update-hosts';
import { MESSAGES, UPDATE_APP_LABELS } from '../../../src/web-server/services/app-update-contract';

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});
const payload = JSON.stringify({
  results: Object.keys(UPDATE_APP_LABELS).map((appId) => ({
    appId,
    status: 'current',
    messageCode: 'current',
  })),
});
async function finish(service: AppUpdateService) {
  for (let i = 0; i < 100 && service.getStatus().job?.state === 'running'; i++)
    await new Promise((resolve) => setTimeout(resolve, 1));
  expect(service.getStatus().job?.state).not.toBe('running');
}
function lock(contents: string, old = true) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-review-lock-'));
  directories.push(home);
  const directory = path.join(home, 'app-updates');
  fs.mkdirSync(directory);
  const file = path.join(directory, 'dashboard-update.lock');
  fs.writeFileSync(file, contents);
  if (old) fs.utimesSync(file, new Date(0), new Date(0));
  return home;
}
it('reclaims old malformed locks but keeps a fresh partial write during its grace', async () => {
  for (const content of ['', '{}', 'not json', 'x'.repeat(600)]) {
    const service = new AppUpdateService({ ccsDir: lock(content), runHost: async () => payload });
    service.start();
    await finish(service);
  }
  const service = new AppUpdateService({ ccsDir: lock('', false), runHost: async () => payload });
  expect(() => service.start()).toThrow(AppUpdateBusyError);
});
it('reclaims a reused PID but never ages out a matching live identity', async () => {
  const stale = new AppUpdateService({
    ccsDir: lock(JSON.stringify({ pid: process.pid, identity: 'previous-boot:1' })),
    identity: () => 'current-boot:100',
    runHost: async () => payload,
  });
  stale.start();
  await finish(stale);
  const live = new AppUpdateService({
    ccsDir: lock(JSON.stringify({ pid: process.pid, identity: 'current-boot:100' })),
    runHost: async () => payload,
    identity: () => 'current-boot:100',
  });
  expect(() => live.start()).toThrow(AppUpdateBusyError);
});
it('strips dashboard secrets from the child boundary and keeps CLI authentication', async () => {
  const env = {
    CCS_SESSION_SECRET: 'private',
    ccs_dashboard_token: 'private',
    DB_PASSWORD_HASH: 'private',
    OPENAI_API_KEY: 'cli-auth',
  };
  expect(updaterEnvironment(env)).toEqual({ OPENAI_API_KEY: 'cli-auth' });
  const result = await runHelperProcess(
    process.execPath,
    [
      '-e',
      `process.stdout.write(JSON.stringify({results: [], safe: !process.env.CCS_SESSION_SECRET && !process.env.ccs_dashboard_token && !process.env.DB_PASSWORD_HASH && process.env.OPENAI_API_KEY === 'cli-auth'}))`,
    ],
    { ...process.env, ...env }
  );
  expect(JSON.parse(result).safe).toBe(true);
});
it('keeps streamed results on a helper protocol failure without calling the host unreachable', async () => {
  const service = new AppUpdateService({
    persist: false,
    runHost: async (platform, control) => {
      if (platform !== 'ubuntu') return payload;
      control.onEvent({
        event: 'result',
        result: {
          appId: 'omp',
          status: 'updated',
          messageCode: 'updated',
          version: '2.0.0',
          manager: 'native',
        },
      });
      throw new HelperExecutionError('helper_invalid');
    },
  });
  service.start();
  await finish(service);
  const rows = service.getStatus().job!.results.filter((row) => row.platform === 'ubuntu');
  expect(rows.find((row) => row.appId === 'omp')!.status).toBe('updated');
  expect(
    rows
      .filter((row) => row.appId !== 'omp')
      .every((row) => row.message === MESSAGES.helper_invalid)
  ).toBe(true);
});
it('does not replace a completed streamed row with a coordinator timeout placeholder', async () => {
  const service = new AppUpdateService({
    persist: false,
    runHost: async (platform, control) => {
      control.onEvent({
        event: 'result',
        result: {
          appId: 'omp',
          status: 'updated',
          messageCode: 'updated',
          version: '2.0.0',
          manager: 'native',
        },
      });
      return JSON.stringify({
        results: Object.keys(UPDATE_APP_LABELS).map((appId) => ({
          appId,
          status: 'unknown',
          messageCode: 'host_timeout',
        })),
      });
    },
  });
  service.start();
  await finish(service);
  expect(
    service
      .getStatus()
      .job!.results.filter((row) => row.appId === 'omp')
      .every((row) => row.status === 'updated')
  ).toBe(true);
});

it('classifies a helper exit without a final document as a protocol failure', async () => {
  await expect(
    runHelperProcess(process.execPath, ['-e', 'process.exit(0)'], process.env)
  ).rejects.toMatchObject({ code: 'helper_invalid' });
});
