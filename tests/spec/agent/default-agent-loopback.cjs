'use strict';
const fs = require('fs'),
  path = require('path'),
  crypto = require('crypto');
const assert = require('node:assert/strict');
const base = path.resolve(__dirname, '../../..');
const root = process.env.AAC_REAL_FIXTURE_ROOT;
const bin = path.join(root, 'bin');
fs.mkdirSync(bin);
const ssh = path.join(bin, 'ssh');
const agent = path.join(root, 'home/.cache/ai-account-center/bin/aac-agent');
fs.mkdirSync(path.dirname(agent), { recursive: true });
fs.writeFileSync(
  agent,
  "#!/usr/bin/python3\nimport sys,json\nr=json.loads(sys.stdin.readline())\nv={'schemaVersion':1,'profiles':[]} if r['verb']=='signin-expiry' else {'id':'plan-kimi-mac','provider':'kimi','platform':'mac','status':'ok','windows':[],'capabilities':{}}\nprint(json.dumps({'protocol':1,'type':'result','id':r['id'],'value':v}))\n",
  { mode: 0o755 }
);
const hash = crypto.createHash('sha256').update(fs.readFileSync(agent)).digest('hex');
const artifacts = [
  { target: 'aarch64-apple-darwin', version: 'fixture', sha256: hash, file: 'unused' },
];
process.env.PATH = bin + ':' + process.env.PATH;
process.env.AAC_AGENT_VERBS = 'on';
process.env.CLAUDE_CONFIG_DIR = path.join(root, 'home/claude');
process.env.CODEX_HOME = path.join(root, 'home/codex');
require('os').homedir = () => process.env.CCS_HOME;
const policy = require(base + '/dist/web-server/services/ssh-policy');
const verbs = require(base + '/dist/web-server/services/agent-verbs');
const { stopProcessBroker } = require(base + '/dist/utils/managed-process');
const original = policy.sshArgv,
  launch = policy.sshLaunch;
let guards = 0;
policy.sshLaunch = async (...args) => {
  guards++;
  return launch(...args);
};
policy.sshArgv = async (...args) => {
  const argv = await original(...args);
  if (argv.includes('KnownHostsCommand=none')) throw Error('expected actual 8.1 capabilities');
  fs.copyFileSync('/usr/bin/ssh', ssh);
  return argv;
};
(async () => {
  const failures = [];
  for (const transport of ['off', 'on'])
    for (const kind of ['direct', 'signin', 'quota']) {
      process.env.AAC_AGENT_TRANSPORT = transport;
      fs.copyFileSync(process.env.AAC_TEST_OPENSSH_81, ssh);
      fs.chmodSync(ssh, 0o755);
      guards = 0;
      let result;
      if (kind === 'direct')
        result = await verbs.runAgentVerb(
          { platform: 'mac', sshHost: 'fixture' },
          'signin-expiry',
          {},
          { artifacts, deadlineMs: 5000, maxBytes: 4096 }
        );
      if (kind === 'signin')
        result = await verbs.runSignInAgent({ platform: 'mac', sshHost: 'fixture' }, artifacts);
      if (kind === 'quota')
        result = await verbs.runQuotaAgent(
          { platform: 'mac', sshHost: 'fixture' },
          { provider: 'kimi', platform: 'mac' },
          artifacts
        );
      console.log(
        JSON.stringify({
          kind,
          transport,
          guards,
          emptyPin: fs.statSync(path.join(root, 'known_hosts')).size === 0,
          result,
        })
      );
      if (result !== null || guards !== 1)
        failures.push(`${transport}/${kind}: guard=${guards}, accepted=${result !== null}`);
      await stopProcessBroker();
    }
  assert.deepEqual(failures, [], 'production defaults must refuse the changed executable');
})().catch(async (e) => {
  console.error(e);
  await stopProcessBroker();
  process.exitCode = 1;
});
