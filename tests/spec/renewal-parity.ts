/**
 * G10 saved-login renewal parity checker.
 *
 *   bun tests/spec/renewal-parity.ts                 # live TS-vs-Rust comparison + goldens
 *   bun tests/spec/renewal-parity.ts --regenerate    # rewrite the checked-in TS goldens
 *
 * Every case (renewal-parity-cases.ts) mirrors a captured TS test. The checker
 * expands the fake-credential sandbox, runs the real TS functions, normalizes
 * the answer (sandbox paths -> <root>, token values -> markers, file versions
 * -> <version>) and compares it byte-for-byte with the Rust runner
 * (crates/aac-renewal, feature parity-fixtures) on identical inputs. No
 * provider calls, no real homes; the Rust side never sees the network.
 */
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { spyOn } from 'bun:test';
import {
  ALPHA,
  BRAVO,
  CHARLIE,
  NOW,
  RenewalSandbox,
  codexProcess,
  fakeFetch,
  issuedSecrets,
  loginBuffer,
  refreshedBody,
  type LoginSpec,
} from '../unit/codex-auth/codex-renewal-fixtures';
import {
  collectCodexRenewalSnapshot,
  planCodexProfileRenewals,
} from '../../src/codex-auth/codex-renewal-planner';
import {
  dryRunCodexProfileRenewal,
  getCodexProfileRenewalStatus,
  renewCodexProfile,
  renewDueCodexProfiles,
  type CodexRenewalCycle,
} from '../../src/codex-auth/codex-profile-renewal';
import { CodexProfileRenewalService } from '../../src/web-server/services/codex-profile-renewal-service';
import {
  parseClaudeSignInRenewal,
  readClaudeSignInRenewal,
} from '../../src/web-server/services/claude-signin-renewal';
import { codexRenewalNote } from '../../src/web-server/services/codex-renewal-notes';
import { readCodexRenewalEntries } from '../../src/web-server/services/codex-renewal-entries';
import { renewalEntry } from '../unit/web-server/codex-renewal-entry-fixture';
import {
  liveLoginToRenew,
  renewLiveLogin,
  sameGrant,
} from '../../src/antigravity/live-login-renewal';
import {
  checkedIdentity,
  credentialFingerprint,
  identityKey,
} from '../../src/antigravity/registry';
import type { NativeCredential, VerifiedIdentity } from '../../src/antigravity/types';
import {
  renewalParityCases,
  type MCase,
  type MLogin,
  type MLoginOrRaw,
} from './renewal-parity-cases';

const repoRoot = path.resolve(fileURLToPath(new URL('../..', import.meta.url)));
const goldenPath = path.join(repoRoot, 'crates/aac-renewal/tests/fixtures/renewal-parity.json');

function toTsLogin(m: MLogin): LoginSpec {
  const account =
    typeof m.account === 'string'
      ? m.account === 'alpha'
        ? ALPHA
        : m.account === 'bravo'
          ? BRAVO
          : CHARLIE
      : {
          email: m.account.email,
          accountId: m.account.account_id,
          userId: m.account.user_id,
        };
  return {
    account,
    refresh: m.refresh,
    label: m.label,
    session: m.session,
    authTime: m.auth_time,
    accessExp: m.access_exp,
    lastRefresh: m.last_refresh,
    accessAccountId: m.access_account_id,
  };
}

const isRaw = (login: MLoginOrRaw): login is { raw: string } => 'raw' in login;
const loginBytes = (login: MLoginOrRaw): Buffer =>
  isRaw(login) ? Buffer.from(login.raw) : loginBuffer(toTsLogin(login));

function buildSandbox(box: RenewalSandbox, sandbox: NonNullable<MCase['sandbox']>): void {
  for (const profile of sandbox.profiles ?? []) {
    if (isRaw(profile.login)) {
      box.writeProfile(profile.name, Buffer.from(profile.login.raw));
      box.registry.createProfile(profile.name);
    } else {
      box.addProfile(profile.name, toTsLogin(profile.login));
    }
  }
  for (const name of sandbox.registryOnly ?? []) box.registry.createProfile(name);
  if (sandbox.live) {
    box.writeLive(isRaw(sandbox.live) ? sandbox.live.raw : toTsLogin(sandbox.live));
  }
  for (const home of sandbox.homes ?? []) {
    box.writeHome(home.rel, isRaw(home.login) ? home.login.raw : toTsLogin(home.login));
  }
  box.processes = (sandbox.processes ?? []).map((process) =>
    codexProcess(process.home ? path.join(box.root, process.home) : null, process.pid ?? 4242)
  );
  if (sandbox.records) {
    fs.mkdirSync(box.ccsDir, { recursive: true });
    fs.writeFileSync(box.statusPath, `${JSON.stringify(sandbox.records, null, 2)}\n`);
  }
  for (const file of sandbox.cliproxy ?? []) {
    const dir = path.join(box.ccsDir, 'cliproxy', file.folder);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, file.name), loginBytes(file.login));
  }
  for (const file of sandbox.extraFiles ?? []) {
    const target = path.join(box.root, file.rel);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, file.raw);
  }
  if (sandbox.lock) fs.mkdirSync(path.join(box.codexHome, '.ccs-activation.lock'));
}

function optionsFor(box: RenewalSandbox, inputs: Record<string, unknown>) {
  return box.options({
    env: (inputs.env as Record<string, string>) ?? {},
    ...(inputs.renewWithinMs !== undefined
      ? { renewWithinMs: inputs.renewWithinMs as number }
      : {}),
    ...(inputs.scan === 'fail'
      ? {
          scanProcesses: () => {
            throw new Error('scan failed');
          },
        }
      : inputs.scan === 'unavailable'
        ? { scanProcesses: () => null }
        : {}),
  });
}

interface MResponse {
  error?: string;
  status?: number;
  body?: unknown;
  spec?: MLogin;
  include_id?: boolean;
  include_refresh?: boolean;
}

function expandResponse(response: MResponse) {
  if (response.error) {
    return Object.assign(new Error(response.error), {
      name: response.error === 'timeout' ? 'TimeoutError' : 'TypeError',
    });
  }
  if (response.spec) {
    return {
      status: 200,
      body: refreshedBody(toTsLogin(response.spec), {
        id: response.include_id !== false,
        refresh: response.include_refresh !== false,
      }),
    };
  }
  return { status: response.status as number, body: response.body };
}

function fetchFor(
  box: RenewalSandbox,
  inputs: Record<string, unknown>,
  actions: { writeProfile?: { name: string; login: MLogin } }[] = []
) {
  const spec = (inputs.fetch ?? {}) as {
    responses?: MResponse[];
    byToken?: Record<string, MResponse>;
    repeatLast?: boolean;
  };
  const responses = (spec.responses ?? []).map(expandResponse);
  const byToken = Object.entries(spec.byToken ?? {}).map(
    ([token, response]) => [token, expandResponse(response)] as const
  );
  let pending = actions;
  return fakeFetch((call) => {
    for (const action of pending) {
      if (action.writeProfile) {
        box.writeProfile(action.writeProfile.name, toTsLogin(action.writeProfile.login));
      }
    }
    pending = [];
    const token = call.body.refresh_token as string;
    const hit = byToken.find(([key]) => key === token) ?? byToken.find(([key]) => key === '*');
    if (hit) return hit[1];
    if (responses.length === 0) throw new Error('unscripted fetch call');
    return spec.repeatLast && responses.length === 1 ? responses[0] : (responses.shift() as never);
  });
}

/** Sort the collections whose order the filesystem decides (readdir order differs across platforms). */
function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      let next = canonicalize(item);
      if (key === 'files' && next && typeof next === 'object' && !Array.isArray(next)) {
        next = Object.fromEntries(
          Object.entries(next).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
        );
      } else if (
        (key === 'sources' || key === 'sharedWith' || key === 'uncheckable') &&
        Array.isArray(next)
      ) {
        const path = (entry: unknown) => {
          if (
            entry &&
            typeof entry === 'object' &&
            'path' in entry &&
            typeof entry.path === 'string'
          ) {
            return entry.path;
          }
          return '';
        };
        next = [...next].sort((a, b) => (path(a) < path(b) ? -1 : path(a) > path(b) ? 1 : 0));
      } else if (key === 'processHomes' && Array.isArray(next)) {
        next = [...next].sort();
      }
      out[key] = next;
    }
    return out;
  }
  return value;
}

function normalize(value: unknown, root: string): unknown {
  if (typeof value === 'string') {
    return value.split(root).join('<root>').replace(/\\/g, '/');
  }
  if (Array.isArray(value)) return value.map((item) => normalize(item, root));
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) out[key] = normalize(item, root);
    return out;
  }
  return value;
}

function normalizeAuth(text: string): string {
  try {
    const parsed = JSON.parse(text) as { tokens?: Record<string, unknown> };
    if (parsed.tokens && typeof parsed.tokens === 'object') {
      for (const [key, marker] of [
        ['id_token', '<id_token>'],
        ['access_token', '<access_token>'],
        ['refresh_token', '<refresh_token>'],
      ]) {
        if (typeof parsed.tokens[key] === 'string') parsed.tokens[key] = marker;
      }
    }
    return JSON.stringify(parsed, null, 2);
  } catch {
    return text;
  }
}

function normalizeStatus(text: string): string {
  try {
    const parsed = JSON.parse(text) as { profiles?: Record<string, { authVersion?: string }> };
    for (const record of Object.values(parsed.profiles ?? {})) {
      if (record && typeof record === 'object' && 'authVersion' in record) {
        record.authVersion = '<version>';
      }
    }
    return `${JSON.stringify(parsed, null, 2)}\n`;
  } catch {
    return text;
  }
}

function filesMap(root: string): Record<string, string> {
  const out: Record<string, string> = {};
  const visit = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile()) {
        const rel = path.relative(root, full).split(path.sep).join('/');
        if (rel.endsWith('codex-profiles.yaml') || rel.startsWith('.ccs/logs/')) continue;
        const text = fs.readFileSync(full, 'utf8');
        out[rel] =
          entry.name === 'auth.json'
            ? normalizeAuth(text)
            : entry.name === 'codex-renewal-status.json'
              ? normalizeStatus(text)
              : text;
      }
    }
  };
  visit(root);
  return out;
}

function credFrom(value: Record<string, unknown>): NativeCredential {
  if (value.empty) return { format: 'native', bytes: Buffer.alloc(0) };
  if (value.oversized) return { format: 'native', bytes: Buffer.alloc(16_385, 'x') };
  if (typeof value.raw === 'string') return { format: 'native', bytes: Buffer.from(value.raw) };
  return {
    format: 'native',
    bytes: Buffer.from(JSON.stringify({ token: { refresh_token: value.refresh } })),
  };
}

function identityFrom(value: Record<string, unknown> | undefined): VerifiedIdentity {
  const v = value ?? {};
  return {
    email: (v.email as string) ?? 'a@example.com',
    subject: (v.subject as string) ?? 'sub-1',
    plan: (v.plan as string | null) ?? null,
    verifiedAt: (v.verifiedAt as string) ?? '2026-10-08T12:00:00.000Z',
    source: (v.source as VerifiedIdentity['source']) ?? 'provider-userinfo',
  };
}

const cycleFixture = (): CodexRenewalCycle => ({
  startedAt: new Date(NOW).toISOString(),
  finishedAt: new Date(NOW).toISOString(),
  enabled: true,
  plans: [],
  attempts: [],
  retryAt: null,
});

async function runKind(testCase: MCase, box: RenewalSandbox): Promise<unknown> {
  const inputs = testCase.inputs ?? {};
  const options = optionsFor(box, inputs);
  switch (testCase.kind) {
    case 'plan': {
      const snapshot = collectCodexRenewalSnapshot(options);
      return Object.fromEntries(
        planCodexProfileRenewals(snapshot, box.clock).map((entry) => [entry.name, entry])
      );
    }
    case 'dryRun':
      return dryRunCodexProfileRenewal(options);
    case 'status':
      return getCodexProfileRenewalStatus(options);
    case 'attempt': {
      const http = fetchFor(box, inputs, (inputs.actions as never) ?? []);
      const attempt = await renewCodexProfile(inputs.name as string, {
        ...options,
        fetch: http.fetch,
      });
      return { attempt, calls: http.calls, files: filesMap(box.root) };
    }
    case 'sequence': {
      const http = fetchFor(box, inputs);
      const cycles: CodexRenewalCycle[] = [];
      for (const step of (inputs.steps as Record<string, unknown>[]) ?? []) {
        box.clock += (step.advanceMs as number) ?? 0;
        for (const action of (step.actions as {
          writeProfile?: { name: string; login: MLogin };
        }[]) ?? []) {
          if (action.writeProfile) {
            box.writeProfile(action.writeProfile.name, toTsLogin(action.writeProfile.login));
          }
        }
        cycles.push(await renewDueCodexProfiles({ ...options, fetch: http.fetch }));
      }
      return {
        cycles,
        status: getCodexProfileRenewalStatus(options),
        calls: http.calls,
        logs: box.logs,
        files: filesMap(box.root),
      };
    }
    case 'service': {
      const service = new CodexProfileRenewalService({
        ccsDir: box.ccsDir,
        env: (inputs.env as Record<string, string>) ?? {},
        now: () => box.clock,
        random: () => 0.5,
        renewal: box.options(),
        runCycle: inputs.failCycle
          ? async () => {
              throw new Error('fixture cycle failure');
            }
          : async () => cycleFixture(),
      });
      const statuses: unknown[] = [];
      for (const step of (inputs.steps as string[]) ?? []) {
        if (step === 'start') service.start();
        else if (step === 'stop') service.stop();
        else if (step === 'status') statuses.push(await service.getStatus());
        else if (step === 'runCycle') await service.runCycle();
      }
      service.stop();
      return statuses;
    }
    case 'claude': {
      const text =
        typeof inputs.text === 'string' ? inputs.text : JSON.stringify(inputs.doc ?? null);
      return parseClaudeSignInRenewal(text, inputs.now as number);
    }
    case 'claudeReader':
      return readClaudeSignInRenewal({
        folder: async () => (inputs.folder as string | null) ?? null,
        readFile: async () => inputs.text as string,
        now: () => inputs.now as number,
      });
    case 'note': {
      const spy = spyOn(Date, 'now').mockReturnValue(inputs.now as number);
      try {
        return codexRenewalNote(
          { needsReauth: !!inputs.needsReauth } as never,
          renewalEntry('gmail', inputs.state as never, inputs.reason as never, {
            accessExpiresAt: (inputs.accessExpiresAt as string) ?? null,
          })
        );
      } finally {
        spy.mockRestore();
      }
    }
    case 'sameGrant':
      return sameGrant(
        credFrom(inputs.left as Record<string, unknown>),
        credFrom(inputs.right as Record<string, unknown>)
      );
    case 'liveToRenew': {
      const credential = inputs.credential
        ? credFrom(inputs.credential as Record<string, unknown>)
        : null;
      const fingerprint = credential ? credentialFingerprint(credential) : null;
      const live = {
        exists: () => inputs.exists !== false,
        read: async () => {
          if (!credential) throw new Error('unreadable');
          return credential;
        },
        validate: async () => {
          if (!inputs.validate || inputs.validate === 'error') throw new Error('cannot verify');
          return identityFrom(inputs.validate as Record<string, unknown>);
        },
        install: async () => undefined,
      };
      const registry = {
        savedRevision: () => (inputs.matchSaved ? fingerprint : null),
        readCredential: () => {
          if (!inputs.saved) throw new Error('missing');
          return { credential: credFrom(inputs.saved as Record<string, unknown>) };
        },
        restoreProfile: () => undefined,
      };
      const profile = {
        id: 'p1',
        identityKey: identityKey(
          checkedIdentity(identityFrom(inputs.profile as Record<string, unknown>))
        ),
      };
      const result = await liveLoginToRenew(live as never, registry as never, profile as never);
      return result ? '<fp>' : null;
    }
    case 'renewLive': {
      const newCredential = credFrom(inputs.newCredential as Record<string, unknown>);
      let liveCredential = inputs.liveAfter
        ? credFrom(inputs.liveAfter as Record<string, unknown>)
        : null;
      const live = {
        exists: () => true,
        read: async () => {
          if (!liveCredential) throw new Error('unreadable');
          return liveCredential;
        },
        validate: async () => {
          throw new Error('cannot verify');
        },
        install: async (credential: NativeCredential) => {
          if (inputs.installOk === false) throw new Error('install failed');
          liveCredential = credential;
        },
      };
      const registry = {
        savedRevision: () => null,
        readCredential: () => {
          throw new Error('missing');
        },
        restoreProfile: () => {
          if (inputs.restoreOk === false) throw new Error('restore failed');
        },
      };
      try {
        return await renewLiveLogin(
          live as never,
          registry as never,
          { index: 0, record: { id: 'p1' } } as never,
          newCredential,
          'fp-old',
          () => {
            if (inputs.saveOk === false) throw new Error('save failed');
          }
        );
      } catch {
        return 'save_error';
      }
    }
    case 'entries': {
      const entries = await readCodexRenewalEntries(async () => {
        if (inputs.fail) throw new Error('read failed');
        return getCodexProfileRenewalStatus(options);
      });
      return Object.fromEntries(entries);
    }
    default:
      throw new Error(`unknown case kind ${testCase.kind}`);
  }
}

/** No fake token text or digest may appear in an answer (refresh markers may: they are request inputs). */
function leakScan(answer: unknown): void {
  const text = JSON.stringify(answer);
  for (const secret of issuedSecrets) {
    if (secret.startsWith('rt-FAKE-')) continue;
    if (text.includes(secret)) {
      throw new Error(`fake credential leaked into parity answer (length ${secret.length})`);
    }
  }
}

async function main(): Promise<void> {
  const regenerate = process.argv.includes('--regenerate');
  const cases = renewalParityCases();
  const tsAnswers = new Map<string, string>();
  const goldens: unknown[] = [];
  for (const testCase of cases) {
    const box = new RenewalSandbox();
    try {
      buildSandbox(box, testCase.sandbox ?? {});
      const answer = await runKind(testCase, box);
      const normalized = canonicalize(normalize(answer, box.root));
      leakScan(normalized);
      tsAnswers.set(testCase.name, JSON.stringify(normalized, null, 2));
      goldens.push({ ...testCase, expected: normalized });
    } finally {
      box.cleanup();
    }
  }
  if (regenerate) {
    fs.writeFileSync(goldenPath, `${JSON.stringify({ version: 1, cases: goldens }, null, 2)}\n`);
    console.log(`renewal parity: wrote ${goldens.length} TS goldens to ${goldenPath}`);
    return;
  }
  let failures = 0;
  const fail = (name: string, detail: string) => {
    failures++;
    console.error(`--- ${name}\n${detail}`);
  };
  const committed = JSON.parse(fs.readFileSync(goldenPath, 'utf8')) as {
    version: number;
    cases: { name: string; expected: unknown }[];
  };
  for (const golden of committed.cases) {
    const ts = tsAnswers.get(golden.name);
    if (ts === undefined) fail(golden.name, 'case missing from the TS case list');
    else if (ts !== JSON.stringify(golden.expected, null, 2)) {
      fail(golden.name, 'TS answer differs from the checked-in golden (regenerate and review)');
    }
  }
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'renewal-parity-'));
  const manifestPath = path.join(tmp, 'manifest.json');
  const answersPath = path.join(tmp, 'answers.json');
  fs.writeFileSync(manifestPath, JSON.stringify({ version: 1, cases }));
  const bin = process.env.AAC_RENEWAL_PARITY_BIN;
  const result = bin
    ? spawnSync(bin, [manifestPath, answersPath], { stdio: 'inherit' })
    : spawnSync(
        'cargo',
        [
          'run',
          '-p',
          'aac-renewal',
          '--features',
          'parity-fixtures',
          '--bin',
          'renewal_parity',
          '--quiet',
          '--',
          manifestPath,
          answersPath,
        ],
        { cwd: repoRoot, stdio: 'inherit' }
      );
  if (result.status !== 0) {
    console.error('renewal parity: the Rust runner failed');
    process.exit(1);
  }
  const rustAnswers = JSON.parse(fs.readFileSync(answersPath, 'utf8')) as Record<string, unknown>;
  for (const testCase of cases) {
    const ts = tsAnswers.get(testCase.name);
    const rust = JSON.stringify(canonicalize(rustAnswers[testCase.name] ?? null), null, 2);
    if (rust !== ts) {
      fail(testCase.name, `Rust differs from TS:\nTS:\n${ts}\nRust:\n${rust}`);
    }
  }
  fs.rmSync(tmp, { recursive: true, force: true });
  if (failures > 0) {
    console.error(`renewal parity: ${failures} case(s) differ`);
    process.exit(1);
  }
  console.log(`renewal parity: ${cases.length} cases byte-equal (TS goldens + live Rust)`);
}

await main();
