/** Max pair 2 M4: the effective client config, never a DNS lookup or connection. */
import { expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { sshArgv, scpArgv } from '../../../src/web-server/services/ssh-policy';

for (const client of [
  '/usr/bin/ssh',
  ...(process.env.AAC_TEST_OPENSSH_81 ? [process.env.AAC_TEST_OPENSSH_81] : []),
])
  it(`disables inherited DNS trust for ssh and scp with ${path.basename(path.dirname(client))}`, async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dns-trust-'));
    const saved = { ...process.env };
    try {
      fs.symlinkSync(client, path.join(dir, 'ssh'));
      Object.assign(process.env, {
        CCS_HOME: dir,
        CCS_DIR: dir,
        PATH: `${dir}:${process.env.PATH}`,
      });
      const config = path.join(dir, 'ssh-config');
      fs.writeFileSync(config, 'Host *\n VerifyHostKeyDNS yes\n');
      const ssh = await sshArgv('fixture.invalid', 'unused');
      const scp = await scpArgv('fixture.invalid', '/fixture/source', 'fixture.invalid:target');
      for (const argv of [ssh, scp]) {
        const options = argv.flatMap((v, i) => (argv[i - 1] === '-o' ? ['-o', v] : []));
        const effective = execFileSync(
          client,
          ['-G', '-F', config, ...options, 'fixture.invalid'],
          { encoding: 'utf8' }
        );
        expect(effective).toContain('verifyhostkeydns false\n');
        expect(effective).toContain('stricthostkeychecking true\n');
        expect(effective).toContain('globalknownhostsfile none\n');
      }
    } finally {
      process.env = saved;
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
