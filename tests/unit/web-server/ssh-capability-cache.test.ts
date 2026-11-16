import { expect, it, setSystemTime } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { scpArgv, sshArgv, sshLaunch } from '../../../src/web-server/services/ssh-policy';
import { runSsh } from '../../../src/web-server/services/usage-scan-binary';
async function fixture(body: string, check: (root: string) => Promise<void>) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-cap-'));
  const saved = { ...process.env };
  try {
    fs.writeFileSync(
      path.join(root, 'ssh'),
      `#!/usr/bin/python3\nimport sys,time\nfrom pathlib import Path\np=Path(__file__).with_name('calls')\nwith p.open('a') as f: f.write('probe\\n')\n${body}\n`,
      { mode: 0o700 }
    );
    process.env.PATH = `${root}:${process.env.PATH}`;
    process.env.CCS_HOME = root;
    process.env.CCS_DIR = root;
    await check(root);
  } finally {
    process.env = saved;
    fs.rmSync(root, { recursive: true, force: true });
  }
}
it('coalesces concurrent no-connect probes and caches an unsupported option', async () => {
  await fixture(
    "time.sleep(.1)\nif 'KnownHostsCommand=none' in sys.argv:\n print('command-line: Bad configuration option: knownhostscommand',file=sys.stderr);sys.exit(255)",
    async (root) => {
      const values = await Promise.all(
        Array.from({ length: 8 }, () => sshArgv('fixture', 'unused'))
      );
      expect(fs.readFileSync(path.join(root, 'calls'), 'utf8').trim().split('\n')).toHaveLength(2);
      for (const argv of values) expect(argv).not.toContain('KnownHostsCommand=none');
      await sshArgv('fixture', 'unused');
      expect(fs.readFileSync(path.join(root, 'calls'), 'utf8').trim().split('\n')).toHaveLength(2);
    }
  );
});
it('bounds and caches failed probes without blocking the JS timer', async () => {
  await fixture(
    'import signal\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\ntime.sleep(5)',
    async (root) => {
      const start = Date.now();
      let timerAt = 0;
      const timer = new Promise<void>((resolve) =>
        setTimeout(() => {
          timerAt = Date.now() - start;
          resolve();
        }, 50)
      );
      const values = await Promise.allSettled(
        Array.from({ length: 3 }, () => sshArgv('fixture', 'unused'))
      );
      await timer;
      expect(values.every((value) => value.status === 'rejected')).toBe(true);
      expect(timerAt).toBeLessThan(500);
      expect(Date.now() - start).toBeLessThan(3000);
      await expect(sshArgv('fixture', 'unused')).rejects.toThrow('SSH capability probe failed');
      expect(fs.readFileSync(path.join(root, 'calls'), 'utf8').trim().split('\n')).toHaveLength(1);
    }
  );
}, 5000);
const oldClient =
  "if 'KnownHostsCommand=none' in sys.argv:\n print('command-line: Bad configuration option: knownhostscommand',file=sys.stderr);sys.exit(255)";
it('re-probes when the executable is replaced at the same path', async () => {
  await fixture(oldClient, async (root) => {
    const calls = () => fs.readFileSync(path.join(root, 'calls'), 'utf8').trim().split('\n').length;
    expect(await sshArgv('fixture', 'unused')).not.toContain('KnownHostsCommand=none');
    expect(calls()).toBe(2);
    const ssh = path.join(root, 'ssh');
    const inode = fs.statSync(ssh).ino;
    // In-place rewrite keeps the inode: size, mtime and ctime still identify the new client.
    fs.writeFileSync(
      ssh,
      "#!/usr/bin/python3\nfrom pathlib import Path\nwith Path(__file__).with_name('calls').open('a') as f: f.write('modern\\n')\n"
    );
    expect(fs.statSync(ssh).ino).toBe(inode);
    expect(await sshArgv('fixture', 'unused')).toContain('KnownHostsCommand=none');
    expect(calls()).toBe(4);
    await sshArgv('fixture', 'unused');
    expect(calls()).toBe(4);
  });
});
it('re-probes when PATH ssh is retargeted and when a failed client is replaced', async () => {
  await fixture('sys.exit(3)', async (root) => {
    const calls = () => fs.readFileSync(path.join(root, 'calls'), 'utf8').trim().split('\n').length;
    await expect(sshArgv('fixture', 'unused')).rejects.toThrow('SSH capability probe failed');
    const ssh = path.join(root, 'ssh');
    fs.renameSync(ssh, path.join(root, 'failed'));
    const write = (name: string, body: string) =>
      fs.writeFileSync(
        path.join(root, name),
        `#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\nwith Path(__file__).with_name('calls').open('a') as f: f.write('${name}\\n')\n${body}\n`,
        { mode: 0o700 }
      );
    write('old', oldClient);
    write('modern', '');
    fs.symlinkSync(path.join(root, 'old'), ssh);
    expect(await sshArgv('fixture', 'unused')).not.toContain('KnownHostsCommand=none');
    fs.unlinkSync(ssh);
    fs.symlinkSync(path.join(root, 'modern'), ssh);
    expect(await sshArgv('fixture', 'unused')).toContain('KnownHostsCommand=none');
    expect(calls()).toBe(5);
  });
});
it('a replacement during the probe is probed again before argv is returned', async () => {
  await fixture(`time.sleep(.3)\n${oldClient}`, async (root) => {
    const pending = sshArgv('fixture', 'unused');
    await new Promise((resolve) => setTimeout(resolve, 100));
    const next = path.join(root, 'next');
    fs.writeFileSync(next, '#!/usr/bin/python3\n', { mode: 0o700 });
    fs.renameSync(next, path.join(root, 'ssh'));
    expect(await pending).toContain('KnownHostsCommand=none');
  });
});
it('scp runs the probed SSH file with the same policy, following a replacement', async () => {
  await fixture(oldClient, async (root) => {
    const ssh = path.join(root, 'ssh');
    const old = await scpArgv('fixture', '/fixture/local', 'fixture:/remote');
    expect(old.slice(0, 3)).toEqual(['-q', '-S', ssh]);
    expect(old).not.toContain('KnownHostsCommand=none');
    for (const option of [
      'StrictHostKeyChecking=yes',
      'GlobalKnownHostsFile=none',
      'ForwardAgent=no',
      'ClearAllForwardings=yes',
      'PermitLocalCommand=no',
    ])
      expect(old).toContain(option);
    expect(old.some((arg) => arg.startsWith('UserKnownHostsFile='))).toBe(true);
    fs.writeFileSync(ssh, '#!/usr/bin/python3\n');
    const modern = await scpArgv('fixture', '/fixture/local', 'fixture:/remote');
    expect(modern.slice(0, 3)).toEqual(['-q', '-S', ssh]);
    expect(modern).toContain('KnownHostsCommand=none');
    expect(modern.slice(-3)).toEqual(['--', '/fixture/local', 'fixture:/remote']);
  });
});
it('a failed probe is retried after its 30 s TTL, so a recovered client works', async () => {
  const failOnce =
    "flag=Path(__file__).with_name('failed-once')\nif not flag.exists():\n flag.write_text('x'); sys.exit(3)";
  await fixture(failOnce, async (root) => {
    const calls = () => fs.readFileSync(path.join(root, 'calls'), 'utf8').trim().split('\n').length;
    try {
      await expect(sshArgv('fixture', 'unused')).rejects.toThrow('SSH capability probe failed');
      await expect(sshArgv('fixture', 'unused')).rejects.toThrow('SSH capability probe failed');
      expect(calls()).toBe(1);
      setSystemTime(new Date(Date.now() + 31_000));
      expect(await sshArgv('fixture', 'unused')).toContain('KnownHostsCommand=none');
      expect(calls()).toBe(3);
      await sshArgv('fixture', 'unused');
      expect(calls()).toBe(3);
    } finally {
      setSystemTime();
    }
  });
});
it('a client replaced after argv is built is refused at launch, never spawned', async () => {
  await fixture(oldClient, async (root) => {
    const ssh = path.join(root, 'ssh');
    const argv = await sshArgv('fixture', 'unused');
    const scp = await scpArgv('fixture', '/fixture/local', 'fixture:/remote');
    expect(await sshLaunch('ssh', argv)).toBe(ssh);
    expect(await sshLaunch('scp', scp)).toBe('scp');
    expect(await sshLaunch('/usr/bin/python3', ['-c', 'pass'])).toBe('/usr/bin/python3');
    fs.writeFileSync(
      ssh,
      "#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\nif '-G' not in sys.argv: Path(__file__).with_name('launched').write_text('x')\n"
    );
    await expect(sshLaunch('ssh', argv)).rejects.toThrow('SSH executable changed before launch');
    await expect(sshLaunch('scp', scp)).rejects.toThrow('SSH executable changed before launch');
    // A production spawn site: refused without starting the replaced client.
    const result = await runSsh(argv, { timeoutMs: 5000 });
    expect(result.code).toBeNull();
    expect(fs.existsSync(path.join(root, 'launched'))).toBe(false);
    // Argv rebuilt for the new client launches that exact file.
    expect(await sshLaunch('ssh', await sshArgv('fixture', 'unused'))).toBe(ssh);
  });
});
