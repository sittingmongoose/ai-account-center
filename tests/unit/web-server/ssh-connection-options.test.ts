import { afterEach, beforeEach, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { sshArgv } from '../../../src/web-server/services/ssh-policy';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { SSH_ALIASES } from '../../../src/web-server/services/dashboard-hosts';

/** The fixed peer aliases (Nas1, the Mac, the two Windows PCs), from the computer registry. */
const peers = Object.values(SSH_ALIASES).filter((alias): alias is string => Boolean(alias));

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

it('resolves direct peer aliases read-only, keeping only endpoint fields and ignoring inherited policy', () => {
  fs.writeFileSync(
    path.join(home, '.ssh/peers'),
    `Host ${peers.join(' ')}
 HostName 127.0.0.1
 User fixture
 Port 2222
 IdentityFile ${home}/fixture-key
 ForwardAgent /fixture/agent.sock
 SendEnv KIMI_API_KEY
 SetEnv FIXTURE_ENV=leaked
 GlobalKnownHostsFile /fixture/alternate-trust
`
  );
  fs.writeFileSync(path.join(home, '.ssh/config'), `Include ${home}/.ssh/peers\n`);
  expect(peers).toHaveLength(4);
  for (const alias of peers) {
    const argv = sshArgv(alias, 'fixture-command');
    expect(argv.slice(0, 3)).toEqual(['-T', '-F', 'none']);
    for (const option of [
      'Hostname=127.0.0.1',
      'User=fixture',
      'Port=2222',
      'ForwardAgent=no',
      'GlobalKnownHostsFile=none',
      'KnownHostsCommand=none',
    ])
      expect(argv).toContain(option);
    expect(argv).toContain(path.join(home, 'fixture-key'));
    expect(argv.join(' ')).not.toMatch(/SendEnv|SetEnv|FIXTURE_ENV|alternate-trust|fixture\/agent/);
    expect(fs.readdirSync(home).sort()).toEqual(['.ssh']);
  }
});

it('refuses unsupported jumps and uses AAC explicit settings before alias discovery', () => {
  fs.writeFileSync(path.join(home, '.ssh/config'), 'Host fixture\n ProxyCommand /bin/false\n');
  expect(() => sshArgv('fixture', 'command')).toThrow('SSH proxy');
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
  const argv = sshArgv('fixture', 'command');
  expect(argv).toContain('Hostname=127.0.0.1');
  expect(argv).toContain(path.join(home, 'explicit-key'));
  expect(argv).toContain(`UserKnownHostsFile="${home}/aac-pin"`);
});

it('refuses an alias with no usable identity and takes the first existing default one', () => {
  // No IdentityFile: ssh -G lists OpenSSH's defaults; none exists here, so nothing is guessed.
  fs.writeFileSync(path.join(home, '.ssh/config'), 'Host fixture\n HostName 127.0.0.1\n');
  expect(() => sshArgv('fixture', 'command')).toThrow('explicit endpoint, identity and pin');
  fs.writeFileSync(path.join(home, '.ssh/id_ed25519'), 'fixture placeholder, not a key\n');
  const argv = sshArgv('fixture', 'command');
  expect(argv[argv.indexOf('-i') + 1]).toBe(path.join(home, '.ssh/id_ed25519'));
});
