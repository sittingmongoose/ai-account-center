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
  it('rejects a huge obsolete raw cache before allocation, and bounds resumable Analytics scans under a 256MB heap', async () => {
    const codexHome = writeCodexFixture();
    const cacheDir = path.join(tempRoot, 'large-legacy-cache');
    fs.mkdirSync(cacheDir, { recursive: true });
    const oldCache = path.join(cacheDir, 'codex-native-usage-v1.json');
    const fd = fs.openSync(oldCache, 'w');
    fs.writeSync(
      fd,
      '{"version":1,"includeCliproxySessions":false,"generatedAt":0,"files":{},"padding":"'
    );
    const padding = Buffer.alloc(1024 * 1024, 32);
    for (let index = 0; index < 160; index++) fs.writeSync(fd, padding);
    fs.writeSync(fd, '"}');
    fs.closeSync(fd);
    const rollout = path.join(codexHome, 'sessions', 'rollout-worker.jsonl');
    const transcript =
      JSON.stringify({
        timestamp: '2026-03-02T10:00:00Z',
        ordinal: 77,
        type: 'response_item',
        payload: { text: 'x'.repeat(12 * 1024 * 1024) },
      }) + '\n';
    fs.appendFileSync(rollout, transcript.repeat(8));
    const request = { kind: 'codex', codexHome, cacheDir };
    const result = (await runNode(`
      const assert=require('node:assert/strict');
      const {loadAccountAnalyticsWorker}=require('./dist/web-server/services/account-analytics-activity');
      (async()=>{
        const legacy=await loadAccountAnalyticsWorker(${JSON.stringify(request)});
        assert.equal(legacy.eventCount,1); assert.equal(legacy.hourly[0].inputTokens,80);
        const request={...${JSON.stringify(request)},activity:{minDate:Date.parse('2026-03-01T00:00:00Z'),
          cacheDir:${JSON.stringify(cacheDir)},maxBytesPerFile:16*1024*1024}};
        const start=Date.now();let result=await loadAccountAnalyticsWorker(request);
        assert.equal(result.scan.complete,false); assert.equal(result.eventCount,1);
        let bytes=result.scan.readBytes,passes=1;
        while(!result.scan.complete&&passes<10){result=await loadAccountAnalyticsWorker(request);bytes+=result.scan.readBytes;passes++;}
        assert.equal(result.scan.complete,true); assert.equal(result.scan.skippedLines,0);
        assert.equal(result.eventCount,1);assert.equal(result.hourly[0].requestCount,1);
        assert.equal(result.hourly[0].inputTokens,80);assert.equal(result.hourly[0].outputTokens,5);
        assert.equal(result.hourly[0].cacheReadTokens,20);
        const warm=await loadAccountAnalyticsWorker(request);assert.equal(warm.scan.readBytes,0);
        assert.deepEqual(warm.hourly,result.hourly);
        console.log('RESULT '+JSON.stringify({passes,bytes,elapsedMs:Date.now()-start,
          warmEvents:warm.eventCount,heapLimitMb:256,complete:result.scan.complete}));
      })().catch(error=>{console.error(error);process.exitCode=1});
    `)) as {
      passes: number;
      bytes: number;
      elapsedMs: number;
      warmEvents: number;
      heapLimitMb: number;
      complete: boolean;
    };
    expect(result.complete).toBe(true);
    expect(result.bytes).toBe(fs.statSync(rollout).size);
    expect(result.passes).toBeGreaterThan(1);
    expect(result.heapLimitMb).toBe(256);
    expect(result.warmEvents).toBe(1);
    expect(result.elapsedMs).toBeLessThan(15000);
  }, 20000);

  it('manually refreshes appended Claude and Codex native logs before the activity cache expires', async () => {
    const codexHome = writeCodexFixture();
    const codexPath = path.join(codexHome, 'sessions', 'rollout-worker.jsonl');
    fs.writeFileSync(
      codexPath,
      codexFixtureLines().join('\n').replaceAll('2026-03-02', '2026-10-01') + '\n'
    );
    const projectsDir = path.join(tempRoot, 'claude-projects');
    fs.mkdirSync(path.join(projectsDir, 'fixture'), { recursive: true });
    const claudePath = path.join(projectsDir, 'fixture', 'usage.jsonl');
    fs.writeFileSync(
      claudePath,
      JSON.stringify({
        type: 'assistant',
        sessionId: 'claude-worker',
        timestamp: '2026-10-01T10:00:00.000Z',
        message: {
          id: 'message-first',
          model: 'claude-sonnet-4-5',
          usage: { input_tokens: 100, output_tokens: 40, cache_read_input_tokens: 9 },
        },
      }) + '\n'
    );
    const requests = [
      { provider: 'claude', request: { kind: 'claude', projectsDir } },
      {
        provider: 'codex',
        request: { kind: 'codex', codexHome, cacheDir: path.join(tempRoot, 'cache') },
      },
    ];
    const appendedClaude = {
      type: 'assistant',
      sessionId: 'claude-worker',
      timestamp: '2026-10-01T10:10:00.000Z',
      message: {
        id: 'message-second',
        model: 'claude-sonnet-4-5',
        usage: { input_tokens: 50, output_tokens: 20, cache_read_input_tokens: 1 },
      },
    };
    const appendedCodex = {
      timestamp: '2026-10-01T10:10:00.000Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: 200,
            cached_input_tokens: 50,
            output_tokens: 10,
            reasoning_output_tokens: 4,
          },
        },
      },
    };
    const result = (await runNode(`
      const fs = require('node:fs');
      const { AccountAnalyticsActivityService } = require('./dist/web-server/services/account-analytics-activity');
      (async () => {
        const now = Date.parse('2026-10-01T16:30:00Z');
        const from = now - 86400000;
        const query = { platform: 'mac', range: '24h', provider: 'all', account: 'all' };
        const service = new AccountAnalyticsActivityService({
          requests: () => ${JSON.stringify(requests)}, now: () => now,
        });
        const before = await service.get(query, from, now);
        fs.appendFileSync(${JSON.stringify(claudePath)}, JSON.stringify(${JSON.stringify(appendedClaude)}) + '\\n');
        fs.appendFileSync(${JSON.stringify(codexPath)}, JSON.stringify(${JSON.stringify(appendedCodex)}) + '\\n');
        const cached = await service.get(query, from, now);
        const refreshed = await service.get({ ...query, refresh: true }, from, now);
        const afterward = await service.get(query, from, now);
        console.log('RESULT ' + JSON.stringify({ before, cached, refreshed, afterward }));
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as {
      before: { totals: { inputTokens: number } };
      cached: { totals: { inputTokens: number } };
      refreshed: {
        status: string;
        providers: Array<{ provider: string; totals: Record<string, number> }>;
        totals: Record<string, number>;
      };
      afterward: { totals: Record<string, number> };
    };
    expect(result.before.totals.inputTokens).toBe(180);
    expect(result.cached.totals).toEqual(result.before.totals);
    expect(result.refreshed.status).toBe('ok');
    expect(result.refreshed.providers.map((row) => [row.provider, row.totals.inputTokens])).toEqual(
      [
        ['claude', 150],
        ['codex', 150],
      ]
    );
    expect(result.refreshed.totals).toMatchObject({
      inputTokens: 300,
      outputTokens: 70,
      cacheReadTokens: 60,
    });
    expect(result.afterward.totals).toEqual(result.refreshed.totals);
  });

  it('returns identical native Codex summaries without transferring raw events', async () => {
    const codexHome = writeCodexFixture([codexFixtureLines()[2], '{malformed']);
    const request = { kind: 'codex', codexHome, cacheDir: path.join(tempRoot, 'native-cache') };
    const result = (await runNode(`
      const assert = require('node:assert/strict');
      const { loadUsageInWorker } = require(${JSON.stringify(clientPath)});
      const { loadAccountAnalyticsWorker } = require('./dist/web-server/services/account-analytics-activity');
      const { scanCodexNativeUsageEntries } = require('./dist/web-server/usage/codex-native-usage-collector');
      const aggregators = require('./dist/web-server/usage/data-aggregator');
      (async () => {
        const result = await loadUsageInWorker(${JSON.stringify(request)});
        const bounded = await loadAccountAnalyticsWorker(${JSON.stringify(request)});
        assert.deepEqual(bounded, result);
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
      inputTokens: 80,
      outputTokens: 5,
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

  it('terminates a deadline-limited analytics worker without returning raw logs or project paths', async () => {
    const codexHome = writeCodexFixture();
    const request = { kind: 'codex', codexHome, cacheDir: path.join(tempRoot, 'native-cache') };
    const result = (await runNode(`
      const { loadAccountAnalyticsWorker } = require('./dist/web-server/services/account-analytics-activity');
      (async () => {
        try { await loadAccountAnalyticsWorker(${JSON.stringify(request)}, 1); throw new Error('Unexpected success'); }
        catch (error) {
          if (error.message === 'Unexpected success') throw error;
          console.log('RESULT ' + JSON.stringify({ bounded: true, message: error.message }));
        }
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `)) as { bounded: boolean; message: string };
    expect(result.bounded).toBe(true);
    expect(result.message).toBe('Local analytics worker could not return bounded usage history');
    expect(result.message).not.toContain(tempRoot);
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
    // Cached input and reasoning output are subsets, rather than extra tokens.
    expect(result).toBeCloseTo((80 * 10 + 5 * 20 + 20 * 5) / 1000000, 10);
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
