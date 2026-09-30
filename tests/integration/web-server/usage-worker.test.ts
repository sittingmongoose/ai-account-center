import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { spawn } from 'child_process';

const repositoryRoot = path.resolve(__dirname, '../../..');
const clientPath = path.join(repositoryRoot, 'dist/web-server/usage/worker-client.js');
let tempRoot = '';

function runNode(code: string): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const child = spawn('node', ['-e', code], {
      cwd: repositoryRoot,
      env: {
        ...process.env,
        CCS_HOME: tempRoot,
        CCS_DIR: path.join(tempRoot, '.ccs'),
        CODEX_HOME: path.join(tempRoot, '.codex'),
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    let errors = '';
    child.stdout.on('data', (data) => (output += data));
    child.stderr.on('data', (data) => (errors += data));
    child.once('error', reject);
    child.once('exit', (code) => {
      if (code !== 0) reject(new Error(`Node worker test failed (${code}): ${errors}`));
      else {
        const result = output.split('\n').find((line) => line.startsWith('RESULT '));
        if (!result) reject(new Error(`Missing worker test result: ${output}`));
        else resolve(JSON.parse(result.slice(7)));
      }
    });
  });
}

function codexFixtureLines(): string[] {
  return [
    JSON.stringify({
      type: 'session_meta',
      payload: { id: 'worker-session', cwd: '/fixture/project', model_provider: 'openai' },
    }),
    JSON.stringify({ type: 'turn_context', payload: { model: 'gpt-5' } }),
    JSON.stringify({
      timestamp: '2026-03-02T10:05:00.000Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 5,
            reasoning_output_tokens: 2,
          },
        },
      },
    }),
  ];
}

function writeCodexFixture(extraLines: string[] = []): string {
  const codexHome = path.join(tempRoot, '.codex');
  const sessions = path.join(codexHome, 'sessions');
  fs.mkdirSync(sessions, { recursive: true });
  fs.writeFileSync(
    path.join(sessions, 'rollout-worker.jsonl'),
    [...codexFixtureLines(), ...extraLines].join('\n') + '\n'
  );
  return codexHome;
}

beforeEach(() => {
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-usage-worker-'));
});

afterEach(() => {
  fs.rmSync(tempRoot, { recursive: true, force: true });
});

describe('compiled Node usage workers', () => {
  it('returns identical native Codex summaries without transferring raw events', async () => {
    const codexHome = writeCodexFixture([codexFixtureLines()[2], '{malformed']);
    const request = { kind: 'codex', codexHome, cacheDir: path.join(tempRoot, 'native-cache') };
    const result = (await runNode(`
      const assert = require('node:assert/strict');
      const { loadUsageInWorker } = require(${JSON.stringify(clientPath)});
      const { scanCodexNativeUsageEntries } = require('./dist/web-server/usage/codex-native-usage-collector');
      const aggregators = require('./dist/web-server/usage/data-aggregator');
      (async () => {
        const result = await loadUsageInWorker(${JSON.stringify(request)});
        const entries = await scanCodexNativeUsageEntries({
          env: { CODEX_HOME: ${JSON.stringify(codexHome)} }, disableCache: true,
        });
        for (const key of ['daily', 'hourly', 'monthly', 'session']) {
          const name = 'aggregate' + key[0].toUpperCase() + key.slice(1) + 'Usage';
          assert.deepEqual(result[key], aggregators[name](entries, 'codex-native'));
        }
        assert.equal('entries' in result, false);
        console.log('RESULT ' + JSON.stringify(result));
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as { eventCount: number; daily: Array<Record<string, unknown>> };

    expect(result.eventCount).toBe(1);
    expect(result.daily[0]).toMatchObject({
      inputTokens: 100,
      outputTokens: 7,
      cacheReadTokens: 20,
      source: 'codex-native',
    });
  });

  it('keeps Claude totals and all four source labels unchanged', async () => {
    const projectsDir = path.join(tempRoot, 'claude-projects');
    fs.mkdirSync(path.join(projectsDir, 'fixture'), { recursive: true });
    fs.writeFileSync(
      path.join(projectsDir, 'fixture', 'usage.jsonl'),
      JSON.stringify({
        type: 'assistant',
        sessionId: 'claude-worker',
        timestamp: '2026-03-02T10:00:00.000Z',
        cwd: '/fixture/claude',
        message: {
          model: 'claude-sonnet-4-5',
          usage: { input_tokens: 100, output_tokens: 40, cache_read_input_tokens: 9 },
        },
      }) + '\n'
    );
    const result = (await runNode(`
      const assert = require('node:assert/strict');
      const { loadUsageInWorker } = require(${JSON.stringify(clientPath)});
      const { loadAllUsageData } = require('./dist/web-server/usage/data-aggregator');
      (async () => {
        const result = await loadUsageInWorker({ kind: 'claude', projectsDir: ${JSON.stringify(projectsDir)} });
        const expected = await loadAllUsageData({ projectsDir: ${JSON.stringify(projectsDir)} });
        for (const key of ['daily', 'hourly', 'monthly', 'session']) assert.deepEqual(result[key], expected[key]);
        const droid = await loadUsageInWorker({ kind: 'droid', homeDir: ${JSON.stringify(tempRoot)} });
        assert.equal(droid.eventCount, 0);
        console.log('RESULT ' + JSON.stringify(result));
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as { eventCount: number; daily: Array<Record<string, unknown>> };
    expect(result.eventCount).toBe(1);
    expect(result.daily[0]).toMatchObject({
      inputTokens: 100,
      outputTokens: 40,
      cacheReadTokens: 9,
    });
  });

  it('rejects collection errors, startup failures and premature worker exits', async () => {
    const exitWorkerPath = path.join(tempRoot, 'exit-worker.js');
    fs.writeFileSync(exitWorkerPath, 'process.exit(7);');
    const result = (await runNode(`
      const { loadUsageInWorker } = require(${JSON.stringify(clientPath)});
      (async () => {
        const errors = [];
        for (const args of [
          [{ kind: 'invalid' }],
          [{ kind: 'claude', projectsDir: '/unused' }, ${JSON.stringify(path.join(tempRoot, 'missing.js'))}],
          [{ kind: 'claude', projectsDir: '/unused' }, ${JSON.stringify(exitWorkerPath)}],
        ]) {
          try { await loadUsageInWorker(...args); errors.push('unexpected success'); }
          catch (error) { errors.push(error.message); }
        }
        console.log('RESULT ' + JSON.stringify(errors));
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as string[];
    expect(result[0]).toContain('Unknown usage worker source');
    expect(result[1]).toContain('Cannot find module');
    expect(result[2]).toContain('exited before returning a result (code 7)');
  });

  it('uses the parent scoped pricing cache instead of the process default', async () => {
    const codexHome = writeCodexFixture();
    const scopedDir = path.join(tempRoot, 'scoped-ccs');
    const result = (await runNode(`
      const { loadUsageInWorker } = require(${JSON.stringify(clientPath)});
      const { runWithScopedConfigDir } = require('./dist/utils/config-manager');
      const { setCachedModelsDevRegistry } = require('./dist/web-server/models-dev/registry-cache');
      (async () => {
        const result = await runWithScopedConfigDir(${JSON.stringify(scopedDir)}, async () => {
          setCachedModelsDevRegistry({ openai: { id: 'openai', models: {
            'gpt-5': { id: 'gpt-5', cost: { input: 10, output: 20, cache_read: 5, cache_write: 0 } },
          } } });
          return loadUsageInWorker({
            kind: 'codex', codexHome: ${JSON.stringify(codexHome)}, cacheDir: ${JSON.stringify(path.join(tempRoot, 'native-cache'))},
          });
        });
        console.log('RESULT ' + JSON.stringify(result.daily[0].totalCost));
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as number;
    expect(result).toBeCloseTo((100 * 10 + 7 * 20 + 20 * 5) / 1000000, 10);
  });

  it('serves independent HTTP requests and timers during a large native history scan', async () => {
    // Large transcript records reproduce expensive JSON parsing without any
    // real account/history data. These records must still be scanned by the
    // production native worker, but never count as usage events.
    const transcript = JSON.stringify({
      type: 'response_item',
      payload: { text: 'x'.repeat(4 * 1024 * 1024) },
    });
    const codexHome = writeCodexFixture(Array.from({ length: 24 }, () => transcript));
    const request = { kind: 'codex', codexHome, cacheDir: path.join(tempRoot, 'native-cache') };
    const result = (await runNode(`
      const http = require('node:http');
      const { loadUsageInWorker } = require(${JSON.stringify(clientPath)});
      (async () => {
        const server = http.createServer((_request, response) => response.end('accounts ready'));
        await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
        const address = server.address();
        let ticks = 0, maxGap = 0, lastTick = Date.now(), finished = false;
        const timer = setInterval(() => { const now = Date.now(); maxGap = Math.max(maxGap, now - lastTick); lastTick = now; ticks++; }, 10);
        const start = Date.now();
        const scan = loadUsageInWorker(${JSON.stringify(request)}).then(result => { finished = true; return result; });
        await new Promise(resolve => setTimeout(resolve, 50));
        const responseStart = Date.now();
        const body = await new Promise((resolve, reject) => {
          http.get('http://127.0.0.1:' + address.port + '/api/codex/profiles', response => {
            let body = ''; response.on('data', chunk => body += chunk); response.on('end', () => resolve(body));
          }).on('error', reject);
        });
        const responseMs = Date.now() - responseStart, pendingAtResponse = !finished;
        const usage = await scan;
        clearInterval(timer);
        await new Promise(resolve => server.close(resolve));
        console.log('RESULT ' + JSON.stringify({ body, responseMs, pendingAtResponse, ticks, maxGap, elapsed: Date.now() - start, eventCount: usage.eventCount }));
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as {
      body: string;
      responseMs: number;
      pendingAtResponse: boolean;
      ticks: number;
      maxGap: number;
      elapsed: number;
      eventCount: number;
    };
    expect(result.body).toBe('accounts ready');
    expect(result.pendingAtResponse).toBe(true);
    expect(result.responseMs).toBeLessThan(250);
    expect(result.maxGap).toBeLessThan(250);
    expect(result.ticks).toBeGreaterThan(10);
    expect(result.eventCount).toBe(1);
  }, 20000);
});
