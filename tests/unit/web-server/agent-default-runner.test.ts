/** Max pair 2 M3: default production wrappers, with a client changed after argv preparation. */
import { it, expect, spyOn, mock } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import * as policy from '../../../src/web-server/services/ssh-policy';
import {
  runAgentVerb,
  runSignInAgent,
  runQuotaAgent,
} from '../../../src/web-server/services/agent-verbs';
import { stopProcessBroker } from '../../../src/utils/managed-process';

const artifacts = [
  { target: 'aarch64-apple-darwin', version: 'fixture', sha256: 'a'.repeat(64), file: 'unused' },
];
for (const transport of ['off', 'on'] as const)
  for (const kind of ['direct', 'signin', 'quota'] as const)
    it(`checks the default ${kind} runner, transport ${transport} (service absent)`, async () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-default-runner-'));
      const saved = { ...process.env };
      const ssh = path.join(dir, 'ssh');
      let guardCalls = 0;
      try {
        Object.assign(process.env, {
          CCS_HOME: dir,
          CCS_DIR: dir,
          AAC_AGENT_VERBS: 'on',
          AAC_AGENT_TRANSPORT: transport,
          PATH: `${dir}:${process.env.PATH}`,
        });
        fs.writeFileSync(
          ssh,
          "#!/usr/bin/python3\nimport sys\nif 'KnownHostsCommand=none' in sys.argv:\n print('Bad configuration option: knownhostscommand',file=sys.stderr);sys.exit(255)\n",
          { mode: 0o700 }
        );
        const realArgv = policy.sshArgv;
        const realLaunch = policy.sshLaunch;
        spyOn(policy, 'sshLaunch').mockImplementation(async (...args) => {
          guardCalls++;
          return realLaunch(...args);
        });
        spyOn(policy, 'sshArgv').mockImplementation(async (...args) => {
          const argv = await realArgv(...args);
          expect(argv).not.toContain('KnownHostsCommand=none');
          // Both clients produce valid accepted DTOs if launched. The changed file is the
          // only reason to refuse. No raw runner is injected into any production wrapper.
          fs.writeFileSync(
            ssh,
            "#!/usr/bin/python3\nimport sys,json\nfrom pathlib import Path\nif '-G' in sys.argv: sys.exit(0)\nPath(__file__).with_name('launched').touch()\nr=json.loads(sys.stdin.readline())\nv={'schemaVersion':1,'profiles':[]} if r['verb']=='signin-expiry' else {'id':'plan-kimi-mac','provider':'kimi','platform':'mac','status':'ok','windows':[],'capabilities':{}}\nprint(json.dumps({'protocol':1,'type':'result','id':r['id'],'value':v}))\n"
          );
          return argv;
        });
        const host = { platform: 'mac' as const, sshHost: 'fixture' };
        const result =
          kind === 'direct'
            ? await runAgentVerb(
                host,
                'signin-expiry',
                {},
                { artifacts, deadlineMs: 1500, maxBytes: 4096 }
              )
            : kind === 'signin'
              ? await runSignInAgent(host as Parameters<typeof runSignInAgent>[0], artifacts)
              : await runQuotaAgent(host, { provider: 'kimi', platform: 'mac' }, artifacts);
        expect(result).toBeNull();
        expect(guardCalls).toBe(1);
        expect(fs.existsSync(path.join(dir, 'launched'))).toBe(false);
      } finally {
        await stopProcessBroker();
        mock.restore();
        process.env = saved;
        fs.rmSync(dir, { recursive: true, force: true });
      }
    }, 10000);
