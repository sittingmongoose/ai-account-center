import { expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { sshArgv } from '../../../src/web-server/services/ssh-policy';
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
