/** L5 fix-round regressions through the actual compiled TS lifecycle and Rust service.
 * Linux fixtures only. No real SSH, credentials, key/trust files or package installs.
 * node tests/spec/agent/lifecycle.mjs <aac-agent> <aac-http> [home|scan|ssh|deadline]
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { spawnSync, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../../..', import.meta.url));
const [agent, binary, selected = 'all'] = process.argv.slice(2);
const keys = ['KIMI_CODE_API_KEY', 'KIMI_API_KEY', 'ZAI_API_KEY', 'ZHIPU_API_KEY',
  'BAILIAN_TOKEN_PLAN_API_KEY', 'OPENCODE_GO_API_KEY', 'OPENCODE_API_KEY'];
if (!process.env.AAC_L5_FIXTURE_CHILD) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-l5-lifecycle-'));
  let result;
  try {
    const pkg = path.join(scratch, 'package');
    fs.mkdirSync(pkg);
    fs.cpSync(path.join(repo, 'dist'), path.join(pkg, 'dist'), { recursive: true,
      filter: (p) => p !== path.join(repo, 'dist/native') });
    fs.cpSync(path.join(repo, 'config'), path.join(pkg, 'config'), { recursive: true });
    fs.mkdirSync(path.join(pkg, 'web-dashboard/public'), { recursive: true });
    fs.copyFileSync(path.join(repo, 'web-dashboard/public/computers.json'), path.join(pkg, 'web-dashboard/public/computers.json'));
    // The packaged process broker: the labelled TS transport runs its commands through it.
    fs.mkdirSync(path.join(pkg, 'scripts'));
    fs.copyFileSync(path.join(repo, 'scripts/process-broker.cjs'), path.join(pkg, 'scripts/process-broker.cjs'));
    fs.symlinkSync(fs.realpathSync(path.join(repo, 'node_modules')), path.join(pkg, 'node_modules'));
    const home = path.join(scratch, 'home');
    fs.mkdirSync(path.join(home, '.ccs'), { recursive: true, mode: 0o700 });
    result = spawnSync(process.execPath, [fileURLToPath(import.meta.url), path.resolve(agent), path.resolve(binary), selected], {
      stdio: 'inherit', timeout: 60_000,
      env: { PATH: process.env.PATH, TMPDIR: process.env.TMPDIR ?? os.tmpdir(),
        HOME: home, USER: 'fixture-user', LOGNAME: 'fixture-user', LANG: 'C.UTF-8', TZ: 'UTC',
        CCS_HOME: home, CCS_DIR: path.join(home, '.ccs'),
        XDG_CONFIG_HOME: path.join(home, '.config'), XDG_DATA_HOME: path.join(home, '.local/share'),
        CODEX_HOME: path.join(home, '.codex'), AAC_AGENT_TRANSPORT: 'on', AAC_AGENT_VERBS: 'on',
        AAC_L5_FIXTURE_CHILD: pkg, AAC_L5_NOT_ALLOWED: 'fixture-only-marker',
        CCS_SESSION_SECRET: 'fixture-only-session-secret',
        ...Object.fromEntries(keys.map((key) => [key, 'fixture-only-provider-key'])) },
    });
    assert.equal(result.status, 0, `lifecycle fixture ${selected} failed`);
  } finally {
    if (result?.status === 0) fs.rmSync(scratch, { recursive: true, force: true });
    else console.error(`lifecycle scratch kept for diagnosis: ${scratch}`);
  }
} else {
  const pkg = process.env.AAC_L5_FIXTURE_CHILD;
  const home = process.env.HOME;
  const ccs = process.env.CCS_DIR;
  const trace = path.join(home, 'trace.jsonl');
  const scenario = path.join(home, 'scenario');
  fs.writeFileSync(scenario, 'agent');
  const identity = JSON.parse(execFileSync(agent, ['--version'], { env: { PATH: process.env.PATH }, encoding: 'utf8' }));
  const artifact = { file: agent, sha256: createHash('sha256').update(fs.readFileSync(agent)).digest('hex'),
    target: identity.target, version: identity.version };
  fs.writeFileSync(path.join(pkg, 'config/agent-artifacts.json'), JSON.stringify({ artifacts: [artifact] }));
  fs.writeFileSync(path.join(ccs, 'config.yaml'), JSON.stringify({ version: 1, agentTransport: 'on', agentVerbs: 'on',
    computers: { local: { label: 'Local fixture', os: 'linux', transport: { kind: 'local' } },
      remote: { label: 'Remote fixture', os: 'linux', sshAlias: 'fixture',
        transport: { kind: 'ssh', host: 'fixture.invalid', key: path.join(home, 'no-key') } } },
    agentTransportSsh: { knownHostsFile: path.join(home, 'no-known-hosts') } }));
  const recorder = path.join(home, 'ssh');
  fs.writeFileSync(recorder, `#!/usr/bin/python3
import sys,os,json,re,shlex,time
if '-G' in sys.argv[1:]: sys.exit(0)  # capability probe, not an agent launcher call
with open(${JSON.stringify(trace)},'a') as f:f.write(json.dumps(sys.argv[1:])+'\\n')
command=shlex.split(sys.argv[-1])[-1]
stage=re.search(r"case '(probe|prepare|commit|run)'",command)
stage=stage[1] if stage else 'run'
assert stage in ('probe','run'),'shipping attempted'
if stage=='run' and open(${JSON.stringify(scenario)}).read()=='hang':time.sleep(30)
os.execv(${JSON.stringify(agent)},[${JSON.stringify(agent)}]+(['--version'] if stage=='probe' else []))
`, { mode: 0o700 });
  process.env.PATH = home + ':' + process.env.PATH;
  process.env.NODE_OPTIONS = '--no-warnings';
  process.env.LD_PRELOAD = '/fixture/not-a-real-library.so';
  const require = createRequire(import.meta.url);
  const { RustService } = require(path.join(pkg, 'dist/web-server/rust-service.js'));
  const { runAgentVerb } = require(path.join(pkg, 'dist/web-server/services/agent-verbs.js'));
  const { runLocalUsageScan } = require(path.join(pkg, 'dist/web-server/services/analytics-remote-transport.js'));
  const { stopProcessBroker } = require(path.join(pkg, 'dist/utils/managed-process.js'));
  const service = new RustService('off');
  const options = { mode: 'on', transport: 'on', deadlineMs: 15_000, maxBytes: 1024 * 1024 };
  const remote = { platform: 'linux', sshHost: 'fixture' };
  const checks = {
    async home() {
      const answer = JSON.parse(await runAgentVerb({ platform: 'linux', sshHost: null }, 'signin-expiry', { nowMs: 1767225600000 }, options));
      assert.equal(answer?.schemaVersion, 1, 'local verb must return an ordinary answer through the real lifecycle env');
      assert.ok(Array.isArray(answer.profiles) && answer.profiles.length > 0);
      const env = Object.fromEntries(fs.readFileSync(`/proc/${service.lease.pid}/environ`, 'utf8').split('\0').filter(Boolean).map((s) => [s.slice(0, s.indexOf('=')), s.slice(s.indexOf('=') + 1)]));
      for (const key of ['HOME', 'USER', 'LOGNAME', 'CCS_HOME', 'CCS_DIR', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'CODEX_HOME', ...keys])
        assert.equal(env[key], process.env[key], `chosen variable ${key} must survive the actual spawn`);
      for (const key of ['AAC_L5_NOT_ALLOWED', 'AAC_L5_FIXTURE_CHILD', 'CCS_SESSION_SECRET', 'NODE_OPTIONS', 'LD_PRELOAD'])
        assert.equal(env[key], undefined, `unlisted variable ${key} must not reach the service`);
    },
    async scan() {
      const answer = await runLocalUsageScan({ kinds: ['codex'], minDateMs: 0, fingerprints: {} }, {
        scanner: { ok: false, cause: 'binary_not_built' },
        agent: (host, verb, args, opts) => {
          assert.equal(opts.deadlineMs, 45_000);
          assert.ok(opts.timeoutMs <= opts.deadlineMs + 5000, 'actual local caller timeout must be admitted');
          return runAgentVerb(host, verb, args, opts);
        },
      });
      assert.equal(answer.reader.name, 'aac-usage-scan');
      assert.equal(answer.kinds.codex.state, 'not_installed');
    },
    async ssh() {
      assert.ok(await runAgentVerb(remote, 'health', {}, options));
      const rows = fs.readFileSync(trace, 'utf8').trim().split('\n').map(JSON.parse);
      const rustPinned = rows.every((argv) => argv.some((v, n) => v === '-o' && argv[n + 1] === 'UpdateHostKeys=no'));
      fs.writeFileSync(trace, '');
      delete process.env.LD_PRELOAD;
      assert.ok(await runAgentVerb(remote, 'health', {}, { ...options, transport: 'off',
        artifacts: [{ ...artifact, target: 'x86_64-unknown-linux-musl' }] }));
      const argv = JSON.parse(fs.readFileSync(trace, 'utf8').trim());
      assert.deepEqual([rustPinned, argv.some((v, n) => v === '-o' && argv[n + 1] === 'UpdateHostKeys=no')],
        [true, true], 'both Rust and TS argv must explicitly forbid host-key updates');
    },
    async deadline() {
      assert.ok(await runAgentVerb(remote, 'health', {}, options)); // warm the registry/client first
      fs.writeFileSync(scenario, 'hang');
      const failures = [];
      const failed = service.transportFailed.bind(service);
      service.transportFailed = (lease, kind) => { failures.push(kind); failed(lease, kind); };
      for (let n = 0; n < 3; n++)
        assert.equal(await runAgentVerb(remote, 'health', {}, { ...options, deadlineMs: 2000, timeoutMs: 2000 }), null);
      assert.deepEqual(failures, [], 'slow agents must decline before the TS timer, without L1 transport failures');
    },
  };
  try {
    await service.start(binary);
    assert.ok(service.lease, 'real lifecycle must boot the service for agents with public reads off');
    for (const [name, check] of Object.entries(checks)) if (selected === 'all' || selected === name) {
      await check(); console.log(`PASS L5 real lifecycle: ${name}`);
    }
  } finally { service.stop(); await stopProcessBroker(); }
}
