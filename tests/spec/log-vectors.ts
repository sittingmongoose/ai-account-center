/**
 * M5 lane K, gap G21: vectors from the TS log (src/services/logging and the request log of
 * request-logging-middleware.ts) for the Rust aac-log crate. Every output is what the real TS
 * code produced on these inputs: string masking, context redaction, argv redaction, URL scrub,
 * whole entries (redaction on and off, stages, latency, errors) and request.completed lines from
 * the real middleware. Ids, clocks, pids and run ids are normalized; everything else is exact.
 *
 *   bun tests/spec/log-vectors.ts            # check tests/spec/log-vectors.json
 *   bun tests/spec/log-vectors.ts --write    # regenerate it (review the diff)
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import express from 'express';

const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-log-vectors-')));
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
process.env.CCS_HOME = root;
process.env.CCS_DIR = path.join(root, '.ccs');
fs.mkdirSync(process.env.CCS_DIR, { mode: 0o700 });
const configPath = path.join(process.env.CCS_DIR, 'config.yaml');
const logPath = path.join(process.env.CCS_DIR, 'logs', 'current.jsonl');

const { maskSecretTokens, redactContext, redactErrorInfo, redactArgv } =
  await import('../../src/services/logging/log-redaction');
const { createLogger } = await import('../../src/services/logging');
const { invalidateLoggingConfigCache } = await import('../../src/services/logging/log-config');
const { scrubLoggedUrl, requestLoggingMiddleware } =
  await import('../../src/web-server/middleware/request-logging-middleware');

const long = 'x'.repeat(2400) + 'é'.repeat(10);
const strings = [
  'plain text',
  'Bearer abcdefghijkl',
  'bearer abcdefghijkl',
  'Bearer short',
  'Bearer abcdefghijklmnop',
  'call failed: Authorization Token abcdefgh12345678 rejected',
  'sk-ant-api03-abcdefghijklmnopqrstuv',
  'key sk-abcdefghijklmnopqrstuvwxyz0123456789 end',
  'ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ',
  'glpat-abcdefghijklmnopqrst',
  'AIzaSyA-abcdefghijklmnopqrstuvwxyz012345',
  'xoxb-1234567890-abcdef',
  'aacd_abcdefghijklmnopqrstu',
  'jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.sig_part-1',
  'https://user:pass@example.com/path?api_key=abcdefgh123',
  'HTTPS://u@host.example/x',
  'url?access_token%3Dabcdefghijk&refresh_token=zzzzzzzzzzz',
  'secret=supersecretvalue',
  long,
  'tabs\tand\nnewlines',
];
const contexts: Record<string, unknown>[] = [
  { method: 'GET', status: 200, ok: true, missing: null },
  { Authorization: 'Bearer abcdefghijkl', cookie: 'a=b', 'set-cookie': ['x'], sid: 12345 },
  {
    refresh_token: 'abc',
    refreshToken: 'abc',
    'refresh-token': 'abc',
    tokenCount: 3,
    token: { nested: 1 },
  },
  {
    mySecretValue: 'x',
    userPassword: 'p',
    'setup-code': '123',
    setupCode: '456',
    passwd_hint: 'h',
  },
  { nested: { deeper: { deepest: { level4: { level5: { level6: 'gone' } } } } } },
  { list: ['Bearer abcdefghijkl', 1, null, { api_key: 'k' }, ['x-api-key']] },
  { message: long, err: { name: 'Error', message: 'failed with sk-ant-abcdefghijklmnopqrst' } },
  {
    'x-goog-api-key': 'g',
    management_key: 'm',
    session_id: 's',
    sessionId: 's',
    'device-token': 'd',
  },
  { url: 'https://user:pass@example.com/x', count: 1.5, big: 12345678901234 },
  {},
];
const argvs: string[][] = [
  ['node', 'cli', '--token', 'abc', '--other', 'x'],
  ['--api-key=secret', '--API_KEY', 'y', '-p', 'prompt text', '--prompt=hello'],
  ['--token'],
  ['--client-secret', 's', '--refresh_token=r', '-P', 'kept', '--auth', 'a'],
];
const urls = [
  '/api/health',
  '/api/auth/devices/me?token=aacd_abcdefghijk',
  '/api/auth/devices/me?token=%61acd_abcdefghijk',
  '/api/auth/devices/me?token=%2561acd_abcdefghijk',
  '/v0/management/auth-files?key=aacu_12345678abcd',
  '/v0/management/x?key=aacu_1234567',
  '/api/accounts/codex%3Awork/reset-claims',
  '/api/accounts/codex:work/reset-claims/prepare?x=1',
  '/api/accounts/a%2Fb/reset-claims%2Fcheck',
  '/API/ACCOUNTS/x/RESET-CLAIMS',
  '/api/accounts/x/reset-claimsy',
  '/api/status?files=%zz',
];

function normalized(line: string): string {
  const entry = JSON.parse(line);
  entry.id = '<id>';
  entry.timestamp = '<time>';
  entry.processId = 0;
  entry.runId = '<run>';
  if (entry.requestId) entry.requestId = '<request>';
  if (entry.context?.requestId) entry.context.requestId = '<request>';
  if (typeof entry.context?.durationMs === 'number') entry.context.durationMs = 0;
  return JSON.stringify(entry);
}

function setLogging(logging: Record<string, unknown>) {
  fs.writeFileSync(
    configPath,
    `version: 10\nlogging:\n${Object.entries(logging)
      .map(([k, v]) => `  ${k}: ${JSON.stringify(v)}`)
      .join('\n')}\n`
  );
  invalidateLoggingConfigCache();
}

function drain(): string[] {
  if (!fs.existsSync(logPath)) return [];
  const lines = fs.readFileSync(logPath, 'utf8').trim().split('\n').filter(Boolean);
  fs.rmSync(logPath);
  return lines.map(normalized);
}

const entryCases = [
  {
    source: 'web-server',
    level: 'info',
    event: 'server.listening',
    message: 'Dashboard server listening',
    context: { host: '127.0.0.1', port: 3000 },
  },
  {
    source: 'rust-service',
    level: 'warn',
    event: 'rust.service.fallback',
    message: 'fell back with Bearer abcdefghijklmn',
    context: contexts[1],
  },
  {
    source: 'codex',
    level: 'error',
    event: 'x.failed',
    message: 'plain',
    context: contexts[4],
    extra: {
      stage: 'cleanup',
      latencyMs: 42,
      error: {
        name: 'ProviderError',
        message: 'token sk-ant-abcdefghijklmnopqrst',
        code: 'E1',
        cause: { name: 'Error', message: 'Bearer abcdefghijkl' },
      },
    },
  },
  { source: 'cli', level: 'debug', event: 'debug.event', message: 'debug only', context: { a: 1 } },
  {
    source: 'web',
    level: 'info',
    event: 'stage.respond',
    message: 'done',
    context: contexts[6],
    extra: { stage: 'respond', latencyMs: 12.5 },
  },
];

async function entries() {
  const out: unknown[] = [];
  for (const redact of [true, false]) {
    for (const level of ['debug', 'warn']) {
      setLogging({ enabled: true, level, redact });
      for (const item of entryCases) {
        const logger = createLogger(item.source);
        const extra = (item as any).extra;
        if (extra)
          logger.stage(extra.stage, item.event, item.message, item.context, {
            level: item.level as any,
            latencyMs: extra.latencyMs,
            error: extra.error,
          });
        else (logger as any)[item.level](item.event, item.message, item.context);
        const lines = drain();
        out.push({ redact, configuredLevel: level, ...item, line: lines[0] ?? null });
      }
    }
  }
  setLogging({ enabled: false });
  createLogger('x').error('e', 'off', {});
  assert.deepEqual(drain(), []);
  return out;
}

async function requests() {
  setLogging({ enabled: true, level: 'info', redact: true });
  const app = express();
  app.use(requestLoggingMiddleware);
  app.use((req, res) => {
    res.status(req.path.includes('auth') ? 401 : 200).json({});
  });
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const port = (server.address() as any).port;
  const out: unknown[] = [];
  for (const [url, agent] of [
    ...urls.map((u) => [u, 'fixture-agent/1.0'] as const),
    ['/api/logs/stream', 'x'] as const,
    ['/api/health', null] as const,
  ]) {
    await new Promise<void>((resolve, reject) => {
      const req = http.request(
        {
          host: '127.0.0.1',
          port,
          path: url,
          method: 'GET',
          headers: agent ? { 'user-agent': agent } : {},
        },
        (res) => {
          res.resume();
          res.on('end', resolve);
        }
      );
      req.on('error', reject);
      req.end();
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    const lines = drain();
    const entry = lines[0] ? JSON.parse(lines[0]) : null;
    out.push({
      url,
      userAgent: agent,
      status: url.includes('auth') ? 401 : 200,
      remoteAddress: entry?.context?.remoteAddress ?? null,
      line: lines[0] ?? null,
    });
  }
  server.close();
  return out;
}

const vectors = {
  mask: strings.map((s) => [s, maskSecretTokens(s)]),
  context: contexts.map((c) => [c, redactContext(c)]),
  error: [[entryCases[2].extra!.error, redactErrorInfo(entryCases[2].extra!.error as any)]],
  argv: argvs.map((a) => [a, redactArgv(a)]),
  url: urls.map((u) => [u, scrubLoggedUrl(u)]),
  entries: await entries(),
  requests: await requests(),
};
fs.rmSync(root, { recursive: true, force: true });
const file = path.join(import.meta.dir, 'log-vectors.json');
const bytes = JSON.stringify(vectors, null, 1) + '\n';
if (process.argv.includes('--write')) fs.writeFileSync(file, bytes);
else
  assert.equal(
    fs.readFileSync(file, 'utf8'),
    bytes,
    'log-vectors.json is stale; regenerate and review'
  );
console.log(
  `log vectors: ${strings.length} strings, ${contexts.length} contexts, ${argvs.length} argv, ${urls.length} urls, ${vectors.entries.length} entries, ${vectors.requests.length} requests`
);
