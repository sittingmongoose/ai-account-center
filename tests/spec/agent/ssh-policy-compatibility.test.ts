import { expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import net from 'net';
import { execFile } from 'child_process';
import { sshArgv } from '../../../src/web-server/services/ssh-policy';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
function run(file: string, args: string[], env = process.env): Promise<string> {
  return new Promise((resolve, reject) =>
    execFile(file, args, { env, timeout: 25000 }, (error, stdout, stderr) =>
      error ? reject(new Error(String(stderr || error))) : resolve(stdout)
    )
  );
}
for (const kind of ['multi-key', 'slow', 'python-multi-key', 'python-slow']) {
  it(`connects through ${kind} SSH config without blocking JS`, async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-compat-'));
    try {
      const output = await run('sudo', [
        '-n',
        'unshare',
        '--mount',
        '--fork',
        'python3',
        path.join(import.meta.dir, 'ssh_compatibility.py'),
        root,
        process.execPath,
        path.join(import.meta.dir, 'ssh-policy-fixture.ts'),
        kind,
      ]);
      expect(JSON.parse(output).connected).toBe(true);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  }, 25000);
}
it.skipIf(!process.env.AAC_TEST_OPENSSH_81)(
  'uses actual OpenSSH 8.1 first on PATH without KnownHostsCommand',
  async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-old-'));
    const saved = { ...process.env };
    try {
      fs.symlinkSync(process.env.AAC_TEST_OPENSSH_81!, path.join(root, 'ssh'));
      process.env.PATH = `${root}:${process.env.PATH}`;
      process.env.CCS_HOME = root;
      process.env.CCS_DIR = root;
      fs.writeFileSync(
        path.join(root, 'config.yaml'),
        JSON.stringify({
          ...createEmptyUnifiedConfig(),
          computers: {
            fixture: {
              transport: {
                host: '127.0.0.1',
                user: 'fixture',
                port: 2222,
                key: path.join(root, 'key'),
              },
            },
          },
        })
      );
      const argv = await sshArgv('fixture', 'unused');
      expect(argv).not.toContain('KnownHostsCommand=none');
      expect(await run('ssh', ['-G', '-F', 'none', ...argv.slice(0, -1)])).toContain(
        'hostname 127.0.0.1'
      );
      // No explicit endpoint: config evaluation belongs to the asynchronous SSH process.
      fs.writeFileSync(path.join(root, 'config.yaml'), JSON.stringify(createEmptyUnifiedConfig()));
      expect(
        await run('ssh', ['-G', '-F', 'none', ...(await sshArgv('fixture', 'unused')).slice(0, -1)])
      ).toContain('hostname fixture');
    } finally {
      process.env = saved;
      fs.rmSync(root, { recursive: true, force: true });
    }
  }
);
it.skipIf(!process.env.AAC_TEST_OPENSSH_81)(
  'an OpenSSH 8.1 client replaced at its path by a modern one is re-probed; the empty pin refuses',
  async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-upgrade-'));
    const bin = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-upgrade-bin-'));
    const saved = { ...process.env };
    const listener = net.createServer();
    await new Promise<void>((resolve) => listener.listen(0, '127.0.0.1', resolve));
    const port = (listener.address() as net.AddressInfo).port;
    await new Promise<void>((resolve) => listener.close(() => resolve()));
    try {
      const ssh = path.join(bin, 'ssh');
      fs.copyFileSync(process.env.AAC_TEST_OPENSSH_81!, ssh);
      fs.chmodSync(ssh, 0o755);
      process.env.PATH = `${bin}:${process.env.PATH}`;
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
      expect(await sshArgv('fixture', 'unused')).not.toContain('KnownHostsCommand=none');
      fs.copyFileSync('/usr/bin/ssh', ssh);
      const argv = await sshArgv('fixture', 'unused');
      expect(argv).toContain('KnownHostsCommand=none');
      const output = await run('sudo', [
        '-n',
        'unshare',
        '--mount',
        '--fork',
        'python3',
        path.join(import.meta.dir, 'ssh_loopback.py'),
        root,
        JSON.stringify(argv.slice(0, -3)),
        String(port),
        ssh,
      ]);
      expect(output).toContain('PASS empty AAC pin');
    } finally {
      process.env = saved;
      fs.rmSync(root, { recursive: true, force: true });
      fs.rmSync(bin, { recursive: true, force: true });
    }
  },
  25000
);
