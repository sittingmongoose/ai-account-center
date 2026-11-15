import { afterEach, beforeEach, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { sshArgv, sshEnvironment } from '../../../src/web-server/services/ssh-policy';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
let home: string;
const saved = { home: process.env.CCS_HOME, dir: process.env.CCS_DIR };
beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-endpoint-'));
  process.env.CCS_HOME = home;
  process.env.CCS_DIR = home;
  fs.mkdirSync(path.join(home, '.ssh'));
});
afterEach(() => {
  if (saved.home === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = saved.home;
  if (saved.dir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = saved.dir;
  fs.rmSync(home, { recursive: true, force: true });
});
it('keeps config identities, jumps and matching in OpenSSH without preflight discovery', async () => {
  fs.writeFileSync(
    path.join(home, '.ssh/config'),
    'Host fixture\n ProxyJump jump\n IdentityFile missing\n IdentityFile wrong\n IdentityFile authorized\n'
  );
  const argv = await sshArgv('fixture', 'command');
  expect(argv).toContain(path.join(home, '.ssh/config'));
  expect(argv).not.toContain('IdentitiesOnly=yes');
  expect(argv).not.toContain('-i');
  expect(argv).toContain('ForwardAgent=no');
  expect(argv).toContain('ForwardX11=no');
  expect(argv).toContain('SetEnv=AAC_SSH_POLICY=1');
});
it('passes partial explicit AAC fields without resolving or collapsing the remaining config', async () => {
  fs.writeFileSync(
    path.join(home, 'config.yaml'),
    JSON.stringify({
      ...createEmptyUnifiedConfig(),
      computers: {
        fixture: {
          transport: {
            host: '127.0.0.1',
            user: 'fixture',
            port: 2222,
            key: path.join(home, 'explicit-key'),
          },
        },
      },
      agentTransportSsh: { knownHostsFile: path.join(home, 'aac-pin') },
    })
  );
  const argv = await sshArgv('fixture', 'command');
  expect(argv).toContain('Hostname=127.0.0.1');
  expect(argv).toContain('User=fixture');
  expect(argv).toContain('Port=2222');
  expect(argv).toContain(path.join(home, 'explicit-key'));
  expect(argv).toContain('IdentitiesOnly=yes');
  expect(argv).toContain(`UserKnownHostsFile="${home}/aac-pin"`);
});
it('allows only connection environment, preserving local authentication with no provider secrets', () => {
  expect(
    sshEnvironment({
      PATH: '/bin',
      HOME: '/fixture',
      LANG: 'C',
      SSH_AUTH_SOCK: '/fixture/agent',
      KIMI_API_KEY: 'fixture-only',
      CCS_SESSION_SECRET: 'fixture-only',
      NODE_OPTIONS: 'fixture-only',
    })
  ).toEqual({ PATH: '/bin', HOME: '/fixture', LANG: 'C', SSH_AUTH_SOCK: '/fixture/agent' });
  expect(
    sshEnvironment(
      {
        Path: 'fixture',
        SystemRoot: 'fixture',
        USERPROFILE: 'fixture',
        KIMI_API_KEY: 'fixture-only',
      },
      true
    )
  ).toEqual({ Path: 'fixture', SystemRoot: 'fixture', USERPROFILE: 'fixture' });
});
