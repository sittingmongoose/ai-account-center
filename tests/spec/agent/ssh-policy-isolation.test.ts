import { expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import net from 'net';
import { execFileSync } from 'child_process';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { sshArgv } from '../../../src/web-server/services/ssh-policy';

it('the production TS SSH argv refuses alternate trust and exports no agent or provider env', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-pin-'));
  const saved = { home: process.env.CCS_HOME, dir: process.env.CCS_DIR };
  const listener = net.createServer();
  await new Promise<void>((resolve) => listener.listen(0, '127.0.0.1', resolve));
  const port = (listener.address() as net.AddressInfo).port;
  await new Promise<void>((resolve) => listener.close(() => resolve()));
  try {
    process.env.CCS_HOME = root;
    process.env.CCS_DIR = root;
    fs.writeFileSync(
      path.join(root, 'config.yaml'),
      JSON.stringify({
        ...createEmptyUnifiedConfig(),
        agentTransportSsh: { knownHostsFile: path.join(root, 'known_hosts') },
        computers: {
          fixture: {
            sshAlias: 'fixture',
            transport: { host: '127.0.0.1', user: 'root', port, key: path.join(root, 'key') },
          },
        },
      })
    );
    const argv = (await sshArgv('fixture', 'unused')).slice(0, -3);
    const output = execFileSync(
      'sudo',
      [
        '-n',
        'unshare',
        '--mount',
        '--fork',
        'python3',
        path.join(import.meta.dir, 'ssh_loopback.py'),
        root,
        JSON.stringify(argv),
        String(port),
      ],
      { encoding: 'utf8' }
    );
    expect(output).toContain('PASS empty AAC pin');
  } finally {
    if (saved.home === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = saved.home;
    if (saved.dir === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = saved.dir;
    fs.rmSync(root, { recursive: true, force: true });
  }
}, 15000);
