/**
 * M4c sign-in session parity (lane G): fake-CLI PTY scripts per provider drive the real
 * TypeScript SignInJobRunner (with the real spawnSignInProcess relay) and the Rust
 * `signin-session` verb on the same fixtures, and the job event sequences must match.
 *
 * The install commit stays in TypeScript until M5, so the Rust session ends at
 * `installPending`; the harness appends the unchanged `complete` tail exactly as the
 * agent-backed runner does, and compares the full sequence with the TS runner's.
 *
 * Run on the build host (Linux or Mac; the PTY relay is Unix):
 *   bun tests/spec/agent/signin-session.mjs /path/to/aac-agent
 *
 * Synthetic credentials only; temporary roots only; no provider is contacted.
 */
import { spawn } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { fileURLToPath } from 'url';
import { SignInJobRunner } from '../../../src/web-server/services/signin-jobs.ts';
import { spawnSignInProcess } from '../../../src/web-server/services/signin-process.ts';

const here = path.dirname(fileURLToPath(import.meta.url));
const agentBin = process.argv[2];
if (!agentBin) {
  console.error('usage: bun tests/spec/agent/signin-session.mjs /path/to/aac-agent');
  process.exit(2);
}
if (process.platform === 'win32') {
  console.error('the PTY relay is Unix-only; run this harness on Nas1 or the Mac');
  process.exit(2);
}

const CODEX_ORIGINS = ['https://auth.openai.com'];
const GOOGLE_ORIGINS = ['https://accounts.google.com'];
const CLAUDE_ORIGINS = ['https://claude.ai'];
const MUSE_ORIGINS = ['https://muse.example'];
const AUTH_CODE = '4/0AbcDEF1234567890';
const FAKE_EMAIL = 'fixture@example.com';
const FAKE_TOKEN = 'ya29.FAKE-TOKEN-0123456789abcdef';
const FAKE_SECRET_LINE = `access_token=${FAKE_TOKEN} email=${FAKE_EMAIL} secret=DO-NOT-LEAK`;

let passed = 0;
let failed = 0;
const failures = [];

function check(name, condition, detail) {
  if (condition) {
    passed += 1;
  } else {
    failed += 1;
    failures.push(`${name}: ${detail ?? 'failed'}`);
    console.error(`FAIL ${name}: ${detail ?? ''}`);
  }
}

function tmpRoot(label) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `aac-signin-${label}-`));
}

/** The fake CLI scripts: one Python program per provider behavior, pidfile first. */
function fakeScript(dir, name, body) {
  const file = path.join(dir, `${name}.py`);
  fs.writeFileSync(file, `import os, sys, time\npidfile = sys.argv[1]\nopen(pidfile, 'w').write(str(os.getpid()))\n${body}\n`, { mode: 0o700 });
  return file;
}

function deviceScreen(url, code) {
  return [
    'print("\\x1b[1mWelcome\\x1b[0m", flush=True)',
    'print("1. Open this link in your browser", flush=True)',
    `print("   \\x1b[94m${url}\\x1b[0m", flush=True)`,
    'print("2. Enter this one-time code", flush=True)',
    `print("   \\x1b[94m${code}\\x1b[0m", flush=True)`,
  ].join('\n');
}

function makeFakes(dir, staging) {
  const pid = (n) => path.join(dir, `${n}.pid`);
  const identity = path.join(staging, 'identity.json');
  const writeIdentity = `open(${JSON.stringify(identity)}, 'w').write('{"accountId":"fixture:work","email":null,"plan":null}')`;
  const fakes = {
    codexSuccess: fakeScript(dir, 'codex-success', `${deviceScreen('https://auth.openai.com/codex/device?state=PRIVATE', 'ABCD-12345')}\nprint(${JSON.stringify(FAKE_SECRET_LINE)}, flush=True)\ntime.sleep(0.15)\n${writeIdentity}\nsys.exit(0)`),
    codexDenied: fakeScript(dir, 'codex-denied', `${deviceScreen('https://auth.openai.com/codex/device', 'ABCD-12345')}\ntime.sleep(0.1)\nsys.exit(1)`),
    codexGarbage: fakeScript(dir, 'codex-garbage', 'print("https://evil.example/steal", flush=True)\ntime.sleep(0.1)\nsys.exit(0)'),
    codexSilent: fakeScript(dir, 'codex-silent', 'time.sleep(0.1)\nsys.exit(0)'),
    codex127: fakeScript(dir, 'codex-127', 'sys.exit(127)'),
    codexSlowFirst: fakeScript(dir, 'codex-slow-first', 'time.sleep(2)\nsys.exit(0)'),
    codexExpire: fakeScript(dir, 'codex-expire', `${deviceScreen('https://auth.openai.com/codex/device', 'ABCD-12345')}\ntime.sleep(5)\nsys.exit(0)`),
    codexSleep: fakeScript(dir, 'codex-sleep', `${deviceScreen('https://auth.openai.com/codex/device', 'ABCD-12345')}\ntime.sleep(30)\nsys.exit(0)`),
    agySuccess: fakeScript(dir, 'agy-success', `print("https://accounts.google.com/o/oauth2/auth?code=PRIVATE", flush=True)\ncode = sys.stdin.readline().strip()\nassert code == ${JSON.stringify(AUTH_CODE)}, code\nprint(${JSON.stringify(FAKE_SECRET_LINE)}, flush=True)\n${writeIdentity}\nsys.exit(0)`),
    agySleep: fakeScript(dir, 'agy-sleep', 'print("https://accounts.google.com/o/oauth2/auth", flush=True)\ntime.sleep(30)\nsys.exit(0)'),
    claudeSuccess: fakeScript(dir, 'claude-success', `print("Opening browser to sign in…", flush=True)\nprint("https://claude.ai/oauth/authorize?code=true&client_id=fixture", flush=True)\ncode = sys.stdin.readline().strip()\nassert code == ${JSON.stringify(AUTH_CODE)}, code\n${writeIdentity}\nsys.exit(0)`),
    museSuccess: fakeScript(dir, 'muse-success', `${deviceScreen('https://muse.example/device', 'WXYZ-67890')}\ntime.sleep(0.1)\n${writeIdentity}\nsys.exit(0)`),
  };
  return { fakes, pid, identity };
}

function commandFor(script, pidfile, { pty, staging }) {
  return {
    file: '/usr/bin/python3',
    args: [script, pidfile],
    env: { PATH: process.env.PATH ?? '/usr/bin:/bin', TERM: 'dumb', NO_COLOR: '1', CODEX_HOME: staging },
    pty,
  };
}

/** The fixture install: read what the fake CLI staged. Identical on both sides. */
function fixtureComplete(identity) {
  return async (_jobId, control) => {
    if (control.stopped()) {
      const err = new Error('stopped');
      err.name = 'SignInJobStopped';
      throw err;
    }
    const raw = JSON.parse(fs.readFileSync(identity, 'utf8'));
    return { accountId: raw.accountId, email: raw.email, plan: raw.plan };
  };
}

function specFor(meta, command, identity, timeoutMs) {
  return {
    provider: meta.provider,
    kind: meta.kind,
    mode: 'add',
    accountId: null,
    profileName: 'work',
    platform: 'ubuntu',
    allowedOrigins: meta.origins,
    timeoutMs,
    prepare: async () => command,
    complete: fixtureComplete(identity),
    cleanup: async () => undefined,
  };
}

/** Normalize a job DTO for sequence comparison: timestamps become placeholders. */
function shape(job) {
  return {
    state: job.state,
    verification: job.verification
      ? { url: job.verification.url, userCode: job.verification.userCode, expiresAt: '<exp>' }
      : null,
    result: job.result,
    error: job.error,
  };
}

function isoMs(value) {
  return Date.parse(value);
}

/** Run the TS reference runner to its terminal state; returns the onChange sequence. */
async function runTs(meta, command, identity, { timeoutMs = 15000, firstOutputTimeoutMs = 30000, act } = {}) {
  const events = [];
  let done = false;
  let last = null;
  const runner = new SignInJobRunner({
    spawn: spawnSignInProcess,
    firstOutputTimeoutMs,
    onChange: (job) => {
      events.push(job);
      last = job;
      if (['succeeded', 'failed', 'expired', 'cancelled'].includes(job.state)) done = true;
    },
  });
  const job = runner.start(specFor(meta, command, identity, timeoutMs));
  const deadline = Date.now() + timeoutMs + 10000;
  let acted = false;
  while (!done && Date.now() < deadline) {
    if (!acted && act && last && act.when(last)) {
      acted = true;
      act.do(runner, job.id);
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  runner.shutdown();
  return { id: job.id, events, done };
}

/** One agent request; returns parsed NDJSON events. Controls are sent by the caller. */
function agentCall(args, { deadlineMs = 30000, onEvent, controls } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(agentBin, [], { stdio: ['pipe', 'pipe', 'pipe'] });
    const id = `parity-${Math.random().toString(16).slice(2, 10)}`;
    const request = { protocol: 1, verb: 'signin-session', id, deadlineMs, args };
    let buffer = '';
    let stderr = '';
    const events = [];
    let terminal = null;
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
    });
    child.stdout.on('data', (chunk) => {
      buffer += chunk;
      let end;
      while ((end = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        if (!line) continue;
        const event = JSON.parse(line);
        events.push(event);
        onEvent?.(event, child);
        if (event.type !== 'progress') terminal = event;
      }
    });
    child.once('error', reject);
    child.once('close', (code) => {
      resolve({ events, terminal, code, stderr, child });
    });
    child.stdin.write(JSON.stringify(request) + '\n');
    if (controls) {
      for (const control of controls) child.stdin.write(JSON.stringify(control) + '\n');
    }
    // stdin stays open for interactive controls; one-shot calls close it.
    if (args.action !== 'start') child.stdin.end();
  });
}

function jobsFromEvents(events) {
  return events.filter((e) => e.type === 'progress').map((e) => e.data.job);
}

/** The adapter's install tail, appended to the Rust session for the full-sequence compare. */
async function appendInstallTail(rustJobs, terminal, identity) {
  const value = terminal?.value;
  if (!value?.installPending) return rustJobs;
  const last = { ...rustJobs[rustJobs.length - 1] };
  const complete = fixtureComplete(identity);
  try {
    const result = await complete(last.id, { stopped: () => false });
    rustJobs.push({
      ...last,
      state: 'succeeded',
      verification: null,
      result,
      error: null,
      updatedAt: new Date().toISOString(),
    });
  } catch (error) {
    rustJobs.push({
      ...last,
      state: 'failed',
      verification: null,
      error: { code: error.code ?? 'write_failed', message: '' },
      updatedAt: new Date().toISOString(),
    });
  }
  return rustJobs;
}

async function compareSequences(name, ts, rustJobs, terminal, identity, { timeoutMs }) {
  const full = await appendInstallTail([...rustJobs], terminal, identity);
  const tsShapes = ts.events.map(shape);
  const rustShapes = full.map(shape);
  check(`${name}/sequence`, JSON.stringify(tsShapes) === JSON.stringify(rustShapes),
    `ts=${JSON.stringify(tsShapes)} rust=${JSON.stringify(rustShapes)}`);
  // Timestamp facts: ISO words, and expiresAt - startedAt == timeoutMs on both sides.
  for (const [side, jobs] of [['ts', ts.events], ['rust', full]]) {
    const first = jobs[0];
    if (!first) continue;
    check(`${name}/${side}/iso`, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(first.startedAt), first.startedAt);
    if (first.expiresAt) {
      check(`${name}/${side}/window`, isoMs(first.expiresAt) - isoMs(first.startedAt) === timeoutMs,
        `${first.expiresAt} - ${first.startedAt} != ${timeoutMs}`);
    }
  }
  check(`${name}/id`, /^job_[a-f0-9]{16}$/.test(ts.id) && /^job_[a-f0-9]{16}$/.test(full[0]?.id ?? ''),
    `${ts.id} vs ${full[0]?.id}`);
}

function scrubCheck(name, events, stderr, secrets) {
  const text = JSON.stringify(events) + stderr;
  for (const secret of secrets) {
    check(`${name}/scrub`, !text.includes(secret), `leaked ${secret.slice(0, 12)}…`);
  }
}

async function sessionCase(name, meta, scriptKey, opts) {
  const dir = tmpRoot(name);
  const staging = path.join(dir, 'staging');
  fs.mkdirSync(staging, { recursive: true });
  const { fakes, pid, identity } = makeFakes(dir, staging);
  const command = commandFor(fakes[scriptKey], pid(name), { pty: opts.pty, staging });
  const timeoutMs = opts.timeoutMs ?? 15000;
  const jobId = `job_${Math.random().toString(16).slice(2, 18).padEnd(16, '0')}`;

  const ts = await runTs(meta, command, identity, {
    timeoutMs,
    firstOutputTimeoutMs: opts.firstOutputTimeoutMs ?? 30000,
    act: opts.actTs,
  });

  let codeSent = false;
  const rust = await agentCall(
    {
      action: 'start',
      jobId,
      ccs: dir,
      provider: meta.provider,
      kind: meta.kind,
      mode: 'add',
      accountId: null,
      profileName: 'work',
      platform: 'ubuntu',
      allowedOrigins: meta.origins,
      timeoutMs,
      firstOutputTimeoutMs: opts.firstOutputTimeoutMs ?? 30000,
      command,
    },
    {
      deadlineMs: timeoutMs + 20000,
      onEvent: (event, child) => {
        if (opts.actRust) opts.actRust(event, child, () => codeSent, (v) => { codeSent = v; });
      },
    }
  );
  const rustJobs = jobsFromEvents(rust.events);
  await compareSequences(name, ts, rustJobs, rust.terminal, identity, { timeoutMs });
  if (opts.scrub) {
    scrubCheck(name, rust.events, rust.stderr, [FAKE_TOKEN, FAKE_EMAIL, 'DO-NOT-LEAK', ...(opts.extraSecrets ?? [])]);
  }
  fs.rmSync(dir, { recursive: true, force: true });
  return { ts, rust };
}

const submitWhenAwaiting = {
  actTs: { when: (job) => job.state === 'awaiting_code', do: (runner, id) => runner.submitCode(id, AUTH_CODE) },
  actRust: (event, child, sent, mark) => {
    if (!sent() && event.type === 'progress' && event.data.job.state === 'awaiting_code') {
      mark(true);
      child.stdin.write(JSON.stringify({ input: AUTH_CODE }) + '\n');
    }
  },
};

const codexMeta = { provider: 'codex', kind: 'device-code', origins: CODEX_ORIGINS };
const agyMeta = { provider: 'antigravity', kind: 'supervised-cli', origins: GOOGLE_ORIGINS };
const claudeMeta = { provider: 'claude', kind: 'supervised-cli', origins: CLAUDE_ORIGINS };
const museMeta = { provider: 'muse', kind: 'device-code', origins: MUSE_ORIGINS };

async function main() {
  // Per-provider success sequences, on both spawn paths (PTY relay and plain pipes).
  await sessionCase('codex-success', codexMeta, 'codexSuccess', { pty: true, scrub: true });
  await sessionCase('agy-success', agyMeta, 'agySuccess', { pty: false, scrub: true, extraSecrets: [AUTH_CODE], ...submitWhenAwaiting });
  await sessionCase('claude-success', claudeMeta, 'claudeSuccess', { pty: true, scrub: true, extraSecrets: [AUTH_CODE], ...submitWhenAwaiting });
  await sessionCase('muse-success', museMeta, 'museSuccess', { pty: false, scrub: true });

  // Failure and timeout vocabulary.
  await sessionCase('codex-denied', codexMeta, 'codexDenied', { pty: true });
  await sessionCase('codex-garbage', codexMeta, 'codexGarbage', { pty: true });
  await sessionCase('codex-silent', codexMeta, 'codexSilent', { pty: true });
  await sessionCase('codex-127', codexMeta, 'codex127', { pty: true });
  await sessionCase('codex-first-output', codexMeta, 'codexSlowFirst', { pty: true, firstOutputTimeoutMs: 300, timeoutMs: 15000 });
  await sessionCase('codex-expire', codexMeta, 'codexExpire', { pty: true, timeoutMs: 600 });

  // Cancel through the control frame: the CLI group must die.
  {
    const name = 'codex-cancel';
    const dir = tmpRoot(name);
    const staging = path.join(dir, 'staging');
    fs.mkdirSync(staging, { recursive: true });
    const { fakes, pid } = makeFakes(dir, staging);
    const command = commandFor(fakes.codexSleep, pid(name), { pty: true, staging });
    const jobId = `job_${Math.random().toString(16).slice(2, 18).padEnd(16, '0')}`;
    const ts = await runTs(codexMeta, command, path.join(staging, 'identity.json'), {
      timeoutMs: 15000,
      act: { when: (job) => job.state === 'waiting', do: (runner, id) => runner.cancel(id) },
    });
    let cancelled = false;
    const rust = await agentCall(
      { action: 'start', jobId, ccs: dir, provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'work', platform: 'ubuntu', allowedOrigins: CODEX_ORIGINS, timeoutMs: 15000, command },
      {
        deadlineMs: 30000,
        onEvent: (event, child) => {
          if (!cancelled && event.type === 'progress' && event.data.job.state === 'waiting') {
            cancelled = true;
            child.stdin.write(JSON.stringify({ cancel: true }) + '\n');
          }
        },
      }
    );
    const rustJobs = jobsFromEvents(rust.events);
    check('codex-cancel/state', rustJobs.at(-1)?.state === 'cancelled', JSON.stringify(rustJobs.at(-1)));
    check('codex-cancel/terminal', rust.terminal?.type === 'failure' && rust.terminal?.failure?.cause === 'cancelled', JSON.stringify(rust.terminal));
    check('codex-cancel/ts-state', ts.events.at(-1)?.state === 'cancelled', ts.events.at(-1)?.state);
    // The second verb call reads the journaled cancel; the fake CLI's pid must be gone.
    const got = await agentCall({ action: 'get', jobId, ccs: dir });
    check('codex-cancel/journal', got.terminal?.value?.job?.state === 'cancelled', JSON.stringify(got.terminal));
    await new Promise((resolve) => setTimeout(resolve, 300));
    const cliPid = Number(fs.readFileSync(pid(name), 'utf8'));
    let alive = true;
    try { process.kill(cliPid, 0); } catch { alive = false; }
    check('codex-cancel/child-dead', !alive, `pid ${cliPid} still alive`);
    scrubCheck('codex-cancel', rust.events, rust.stderr, [FAKE_TOKEN]);
    fs.rmSync(dir, { recursive: true, force: true });
  }

  // Cancel from a second verb call (journal flag + group kill), then verify the child.
  {
    const name = 'second-call-cancel';
    const dir = tmpRoot(name);
    const staging = path.join(dir, 'staging');
    fs.mkdirSync(staging, { recursive: true });
    const { fakes, pid } = makeFakes(dir, staging);
    const command = commandFor(fakes.codexSleep, pid(name), { pty: true, staging });
    const jobId = `job_${Math.random().toString(16).slice(2, 18).padEnd(16, '0')}`;
    let waiting = null;
    const session = agentCall(
      { action: 'start', jobId, ccs: dir, provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'work', platform: 'ubuntu', allowedOrigins: CODEX_ORIGINS, timeoutMs: 15000, command },
      { deadlineMs: 30000, onEvent: (event) => { if (event.type === 'progress' && event.data.job.state === 'waiting') waiting = true; } }
    );
    while (!waiting) await new Promise((resolve) => setTimeout(resolve, 20));
    const cancel = await agentCall({ action: 'cancel', jobId, ccs: dir }, { deadlineMs: 10000 });
    check('second-call-cancel/accepted', cancel.terminal?.value?.cancelled === true, JSON.stringify(cancel.terminal));
    const ended = await session;
    check('second-call-cancel/state', jobsFromEvents(ended.events).at(-1)?.state === 'cancelled', JSON.stringify(ended.terminal));
    const got = await agentCall({ action: 'get', jobId, ccs: dir });
    check('second-call-cancel/journal', got.terminal?.value?.job?.state === 'cancelled', JSON.stringify(got.terminal));
    const cliPid = Number(fs.readFileSync(pid(name), 'utf8'));
    let alive = true;
    try { process.kill(cliPid, 0); } catch { alive = false; }
    check('second-call-cancel/child-dead', !alive, `pid ${cliPid} still alive`);
    fs.rmSync(dir, { recursive: true, force: true });
  }

  // A killed agent (TS restart): the journal recovers the job and kills the stray CLI.
  {
    const name = 'restart-recovery';
    const dir = tmpRoot(name);
    const staging = path.join(dir, 'staging');
    fs.mkdirSync(staging, { recursive: true });
    const { fakes, pid } = makeFakes(dir, staging);
    const command = commandFor(fakes.codexSleep, pid(name), { pty: true, staging });
    const jobId = `job_${Math.random().toString(16).slice(2, 18).padEnd(16, '0')}`;
    let child = null;
    let waiting = false;
    const session = agentCall(
      { action: 'start', jobId, ccs: dir, provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'work', platform: 'ubuntu', allowedOrigins: CODEX_ORIGINS, timeoutMs: 15000, command },
      { deadlineMs: 30000, onEvent: (event, c) => { child = c; if (event.type === 'progress' && event.data.job.state === 'waiting') waiting = true; } }
    );
    while (!waiting) await new Promise((resolve) => setTimeout(resolve, 20));
    await new Promise((resolve) => setTimeout(resolve, 400)); // let the journal name the CLI pid
    child.kill('SIGKILL');
    await session;
    const got = await agentCall({ action: 'get', jobId, ccs: dir }, { deadlineMs: 10000 });
    const job = got.terminal?.value?.job;
    check('restart-recovery/state', job?.state === 'failed' && job?.error?.code === 'server_restarted', JSON.stringify(job));
    await new Promise((resolve) => setTimeout(resolve, 300));
    const cliPid = Number(fs.readFileSync(pid(name), 'utf8'));
    let alive = true;
    try { process.kill(cliPid, 0); } catch { alive = false; }
    check('restart-recovery/child-dead', !alive, `pid ${cliPid} still alive`);
    fs.rmSync(dir, { recursive: true, force: true });
  }

  // The install settle and the journal's read semantics.
  {
    const name = 'settle-and-get';
    const dir = tmpRoot(name);
    const staging = path.join(dir, 'staging');
    fs.mkdirSync(staging, { recursive: true });
    const { fakes, identity } = makeFakes(dir, staging);
    const command = commandFor(fakes.codexSuccess, path.join(dir, 'settle.pid'), { pty: true, staging });
    const jobId = `job_${Math.random().toString(16).slice(2, 18).padEnd(16, '0')}`;
    const rust = await agentCall(
      { action: 'start', jobId, ccs: dir, provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'work', platform: 'ubuntu', allowedOrigins: CODEX_ORIGINS, timeoutMs: 15000, command },
      { deadlineMs: 30000 }
    );
    check('settle/install-pending', rust.terminal?.value?.installPending === true, JSON.stringify(rust.terminal));
    const settled = await agentCall({
      action: 'settle', jobId, ccs: dir, state: 'succeeded',
      result: { accountId: 'codex:work', email: null, plan: null }, error: null,
    });
    check('settle/state', settled.terminal?.value?.job?.state === 'succeeded', JSON.stringify(settled.terminal));
    const got = await agentCall({ action: 'get', jobId, ccs: dir });
    check('settle/get', got.terminal?.value?.job?.result?.accountId === 'codex:work', JSON.stringify(got.terminal));
    const unknown = await agentCall({ action: 'get', jobId: 'job_ffffffffffffffff', ccs: dir });
    check('get/placeholder', unknown.terminal?.value?.job?.error?.code === 'server_restarted' && unknown.terminal?.value?.job?.startedAt === null, JSON.stringify(unknown.terminal));
    // The journal's per-provider limit: a second codex start conflicts while one runs.
    const command2 = commandFor(fakes.codexSleep, path.join(dir, 'limit.pid'), { pty: true, staging });
    let waiting = false;
    const first = agentCall(
      { action: 'start', jobId: 'job_1111111111111111', ccs: dir, provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'work', platform: 'ubuntu', allowedOrigins: CODEX_ORIGINS, timeoutMs: 15000, command: command2 },
      { deadlineMs: 30000, onEvent: (event) => { if (event.type === 'progress' && event.data.job.state === 'waiting') waiting = true; } }
    );
    while (!waiting) await new Promise((resolve) => setTimeout(resolve, 20));
    const second = await agentCall(
      { action: 'start', jobId: 'job_2222222222222222', ccs: dir, provider: 'codex', kind: 'device-code', mode: 'add', accountId: null, profileName: 'work', platform: 'ubuntu', allowedOrigins: CODEX_ORIGINS, timeoutMs: 15000, command: command2 },
      { deadlineMs: 10000 }
    );
    check('limits/job-running', second.terminal?.value?.conflict?.code === 'job_running', JSON.stringify(second.terminal));
    const cancel = await agentCall({ action: 'cancel', jobId: 'job_1111111111111111', ccs: dir }, { deadlineMs: 10000 });
    check('limits/cleanup', cancel.terminal?.value?.cancelled === true, JSON.stringify(cancel.terminal));
    await first;
    fs.rmSync(dir, { recursive: true, force: true });
  }

  console.log(`signin-session parity: ${passed} passed, ${failed} failed`);
  if (failed > 0) {
    console.error(failures.join('\n'));
    process.exit(1);
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
