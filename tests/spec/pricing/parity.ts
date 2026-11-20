/**
 * G15 pricing parity corpus: cases.json, generated from the TypeScript modules themselves.
 *
 * Every case records what the TS pricing stack (model-pricing.ts over model-pricing-data.ts, the
 * models.dev registry cache and account-analytics-pricing.ts) answers for its inputs, as the
 * exact `JSON.stringify` text; the Rust port (crates/aac-views/src/pricing) must produce the same
 * bytes for the same case (`crates/aac-views/tests/pricing_spec.rs`). Recorded fixtures only —
 * no live models.dev fetch; the refresh cases replay fetch outcomes through the injected
 * `fetchImpl`, exactly like tests/unit/models-dev-registry-cache.test.ts.
 *
 * Regenerate (only together with a deliberate pricing change):
 *   bun tests/spec/pricing/parity.ts --write
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { getModelPricingWithSource, calculateCost } from '../../../src/web-server/model-pricing';
import { getModelPricing } from '../../../src/web-server/model-pricing';
import { getKnownModels } from '../../../src/web-server/model-pricing';
import {
  clearModelsDevRegistryCache,
  getCachedModelsDevRegistry,
  refreshModelsDevRegistry,
  setCachedModelsDevRegistry,
} from '../../../src/web-server/models-dev/registry-cache';
import { defaultAccountAnalyticsPricing } from '../../../src/web-server/services/account-analytics-pricing';

const corpusPath = path.join(import.meta.dir, 'cases.json');

// ------------------------------------------------------------------ fixtures

/** The models.dev cache the unit tests use, as a raw registry payload. */
const MAIN_FIXTURE = {
  openai: {
    id: 'openai',
    name: 'OpenAI',
    models: {
      'gpt-5.5': { id: 'gpt-5.5', name: 'GPT-5.5', cost: { input: 5, output: 30, cache_read: 0.5 } },
      'gpt-4o': { id: 'gpt-4o', name: 'GPT-4o', cost: { input: 2.5, output: 10, cache_read: 1.25 } },
      'openai-exclusive-model': { id: 'openai-exclusive-model', name: 'OpenAI Exclusive Model', cost: { input: 9, output: 18 } },
    },
  },
  'github-copilot': {
    id: 'github-copilot',
    name: 'GitHub Copilot',
    models: {
      'gpt-5.5': { id: 'gpt-5.5', name: 'GPT-5.5', cost: { input: 0, output: 0 } },
      'gpt-4o': { id: 'gpt-4o', name: 'GPT-4o', cost: { input: 0, output: 0 } },
    },
  },
  google: {
    id: 'google',
    name: 'Google',
    models: {
      'gemini-3-flash-preview': {
        id: 'gemini-3-flash-preview',
        name: 'Gemini 3 Flash Preview',
        cost: { input: 99, output: 99 },
      },
    },
  },
};

/** The Qwen token-plan route the policy aliases must beat, plus malformed entries to drop. */
const TOKEN_PLAN_FIXTURE = {
  'alibaba-token-plan': {
    id: 'alibaba-token-plan',
    name: 'Alibaba Token Plan',
    models: {
      'qwen3.8-max': { id: 'qwen3.8-max', cost: { input: 0, output: 0 } },
      'qwen3.8-flash': { id: 'qwen3.8-flash', cost: { input: 0, output: 0 } },
      'qwen3.7-plus': { id: 'qwen3.7-plus', cost: { input: 0, output: 0 } },
      'null-entry': null,
      'string-entry': 'bad',
    },
  },
  ignored: { id: 'ignored' },
  'aliased-key': {
    name: 'Key stands in for the id',
    models: {
      '  ': { cost: { input: 1, output: 2 } },
      'Date-Suffix-Model': { id: 'date-suffix-model-20260101', cost: { input: 7, output: 8, cache_write: 9, cache_read: 'nan-string' } },
    },
  },
};

/** Providers that disagree on a model's price: the unambiguous path must refuse it. */
const AMBIGUOUS_FIXTURE = {
  first: {
    id: 'first',
    models: { 'shared-model': { id: 'shared-model', cost: { input: 1, output: 1 } } },
  },
  second: {
    id: 'second',
    models: { 'shared-model': { id: 'shared-model', cost: { input: 2, output: 2 } } },
  },
  third: {
    id: 'third',
    models: {
      'shared-model': { id: 'shared-model', cost: { input: 1, output: 1 } },
      'agreed-model': { id: 'agreed-model', cost: { input: 3, output: 4, cache_write: 5 } },
      'claude-opus-4-6': { id: 'claude-opus-4-6', cost: { input: 999, output: 999 } },
    },
  },
  fourth: {
    id: 'fourth',
    models: { 'agreed-model': { id: 'agreed-model', cost: { input: 3, output: 4, cache_write: 5 } } },
  },
};

const FIXTURES: Record<string, object> = {
  main: MAIN_FIXTURE,
  'token-plan': TOKEN_PLAN_FIXTURE,
  ambiguous: AMBIGUOUS_FIXTURE,
};

// ------------------------------------------------------------------ corpus

interface LookupCase {
  name: string;
  fixture: string | null;
  fetchedAt: number;
  model: string;
  provider?: string;
  serviceTier?: string;
  usage?: [number, number, number, number];
  cacheBytes: string;
  resolution: string;
  analytics: string;
  cost?: string;
}

interface ReadCase {
  name: string;
  /** Raw cache-file text to install (not necessarily a TS-written cache). */
  fileText: string;
  now: number;
  allowStale: boolean;
  reads: boolean;
}

type FetchScript =
  | { kind: 'ok'; body: string }
  | { kind: 'http-error'; body: string }
  | { kind: 'transport-error' };

interface RefreshStep {
  fetch: FetchScript;
  force: boolean;
  now: number;
  /** JSON.stringify of the returned registry, or 'null'. */
  returned: string;
  /** Cache-file bytes after the step (file may be absent: 'absent'). */
  cacheBytes: string;
  fetchesSoFar: number;
}

interface RefreshCase {
  name: string;
  steps: RefreshStep[];
}

interface FactsCase {
  name: string;
  fixture: string;
  models: string[];
  routes: string[];
  table: string;
}

interface Corpus {
  version: 1;
  fixtures: Record<string, object>;
  lookups: LookupCase[];
  reads: ReadCase[];
  refreshes: RefreshCase[];
  facts: FactsCase[];
  knownModels: { fixture: string; models: string } | null;
}

const GOLDEN = JSON.parse(
  fs.readFileSync(path.join(import.meta.dir, '../../unit/model-pricing-table-parity.golden.json'), 'utf8')
) as {
  lookups: Record<string, unknown>;
  tierLookups: Record<string, { fast: unknown; unknownTier: unknown }>;
};

function stringify(value: unknown): string {
  return JSON.stringify(value ?? null);
}

function installFixture(name: string | null, fetchedAt: number): string {
  clearModelsDevRegistryCache();
  if (name === null) return 'absent';
  setCachedModelsDevRegistry(FIXTURES[name] as never, fetchedAt);
  return fs.readFileSync(path.join(ccsDir(), 'models-dev-registry-cache.json'), 'utf8');
}

let ccsHome = '';

function ccsDir(): string {
  return path.join(ccsHome, '.ccs');
}

function lookupCase(
  name: string,
  model: string,
  options: { fixture?: string | null; fetchedAt?: number; provider?: string; serviceTier?: string; usage?: [number, number, number, number] } = {}
): LookupCase {
  const fixture = options.fixture ?? null;
  const fetchedAt = options.fetchedAt ?? 123;
  const cacheBytes = installFixture(fixture, fetchedAt);
  const provider = options.provider;
  const serviceTier = options.serviceTier;
  const resolution = JSON.stringify(getModelPricingWithSource(model, { provider, serviceTier }));
  const analytics = stringify(defaultAccountAnalyticsPricing(model, provider));
  const cost = options.usage
    ? JSON.stringify(calculateCost(
        {
          inputTokens: options.usage[0],
          outputTokens: options.usage[1],
          cacheCreationTokens: options.usage[2],
          cacheReadTokens: options.usage[3],
        },
        model,
        { provider, serviceTier }
      ))
    : undefined;
  return { name, fixture, fetchedAt, model, ...(provider !== undefined && { provider }), ...(serviceTier !== undefined && { serviceTier }), ...(options.usage && { usage: options.usage }), cacheBytes, resolution, analytics, ...(cost !== undefined && { cost }) };
}

function buildLookups(): LookupCase[] {
  const cases: LookupCase[] = [];

  // 1. Every golden lookup id (the pre-split capture: table ids and their variant spellings).
  for (const model of Object.keys(GOLDEN.lookups)) {
    cases.push(lookupCase(`golden:${model}`, model));
  }
  // 2. Service tiers, fast and unknown.
  for (const model of Object.keys(GOLDEN.tierLookups)) {
    cases.push(lookupCase(`tier-fast:${model}`, model, { serviceTier: 'fast' }));
    cases.push(lookupCase(`tier-unknown:${model}`, model, { serviceTier: 'no-such-tier' }));
  }
  cases.push(lookupCase('tier-priority:minimax', 'MiniMax-M3', { serviceTier: 'priority' }));
  cases.push(lookupCase('tier-empty:minimax', 'MiniMax-M3', { serviceTier: '' }));

  // 3. models.dev integration over the main fixture (the unit-test behaviours).
  const main = (model: string, provider?: string, serviceTier?: string, usage?: [number, number, number, number], name = model) =>
    cases.push(lookupCase(`main:${name}`, model, { fixture: 'main', provider, serviceTier, usage }));
  main('openai/gpt-5.5');
  main('gpt-5.5');
  main('gpt-5.5', 'openai');
  main('gpt-5.5', 'ghcp', undefined, [1e6, 1e6, 1e6, 1e6]);
  main('gpt-5.5', 'github-copilot');
  main('gpt-4o');
  main('gpt-4o', 'github-copilot');
  main('gemini-3-flash-preview', 'google');
  main('openai-exclusive-model', 'github-copilot');
  main('github-copilot/openai-exclusive-model');
  main('OPENAI/GPT-5.5');
  main(' openai/gpt-5.5 ');
  main('gpt-5.5:session-suffix');
  main('gpt-5.5:openai/gpt-5.5');

  // 4. Policy aliases beating the token-plan $0 route, malformed entries dropped.
  const token = (model: string, provider?: string) =>
    cases.push(lookupCase(`token-plan:${model}${provider ? `@${provider}` : ''}`, model, { fixture: 'token-plan', provider }));
  token('qwen3.8-max');
  token('qwen3.8-max', 'alibaba-token-plan');
  token('qwen3.8-flash', 'alibaba-token-plan');
  token('qwen3.7-plus', 'qwen');
  token('shared-nothing', 'alibaba-token-plan');
  token('date-suffix-model', 'aliased-key');
  token('date-suffix-model-20260101', 'aliased-key');
  token('', 'aliased-key');

  // 5. Ambiguity: same price everywhere resolves to the first provider; disagreement refuses.
  const ambiguous = (model: string, provider?: string) =>
    cases.push(lookupCase(`ambiguous:${model}${provider ? `@${provider}` : ''}`, model, { fixture: 'ambiguous', provider }));
  ambiguous('agreed-model');
  ambiguous('shared-model');
  ambiguous('shared-model', 'first');
  ambiguous('shared-model', 'second');
  ambiguous('claude-opus-4-6');
  ambiguous('claude-opus-4-6', 'claude');
  ambiguous('claude-opus-4-6', 'third');

  // 6. Provider contexts over the static table (aliases, routed and unrouted).
  const routed = (model: string, provider: string) =>
    cases.push(lookupCase(`routed:${model}@${provider}`, model, { provider }));
  for (const provider of [
    'anthropic', 'zai', 'z-ai', 'alibaba-token-plan', 'kimi-code', 'google-antigravity', 'vllm',
    'flashnext', 'openrouter', 'muse-code', 'opencode-go', 'ghcp', 'github', 'copilot', 'kimi',
    'moonshot', 'qwen', 'gemini', 'agy', 'antigravity', 'claude', 'codex', 'openai', 'unknown-provider',
  ]) {
    routed('claude-opus-5-5', provider);
  }
  routed('glm-5.3', 'zai');
  routed('kimi-k3', 'kimi-code');
  routed('k3', 'omp');
  routed('muse-spark-1.3-contributor', 'muse-code');
  routed('qwen3.8-27b', 'vllm');
  routed('stealth', 'flashnext');
  routed('union-alpha', 'openrouter');
  routed('minimax-m2.5', 'minimax');
  routed('MiniMax-M2.1-lightning', 'MiniMax');
  routed('claude-sonnet-4-5', 'anthropic');
  routed('claude-sonnet-4-5', '');

  // 7. Spelling variants the candidate lists must survive.
  const spelled = (model: string) => cases.push(lookupCase(`spelled:${model}`, model));
  for (const model of [
    'claude-opus-4-6-20260101', 'claude-opus-4-6-20260101-thinking', 'claude-opus-4-6-20260101:session',
    'anthropic/claude-opus-4-6-20260101', 'anthropic/claude-opus-4-6-20260101-thinking',
    'claude-opus-4-7-20260401', 'claude-opus-4-8-20260530', 'claude-opus-5-20270101',
    'claude-opus-5-high', 'claude-opus-5-xhigh', 'claude-opus-5-5-max', 'claude-sonnet-5-5-low',
    'claude-opus-4-6-medium-thinking', 'claude-opus-5-thinking-high',
    'claude-aac-unlisted-9', 'claude-aac-unlisted-9-high', 'claude-opus-6-20270101', 'gemini-3.2-pro',
    'claude-sonnet-4-5-thinking:session-1', 'CLAUDE-OPUS-4-6', '  claude-opus-4-6  ',
    'qwen3-32b-20260101', 'qwen3-coder', 'gemini-3.1-flash-preview-customtools', 'gemini-3-1-pro-preview',
    'proxy-gpt-4o-mini', 'router-glm-4.6', 'unknown-model-xyz', '/leading-slash', 'a/', '//',
  ]) {
    spelled(model);
  }
  cases.push(lookupCase('spelled:claude-opus-4-8-fast-dated', 'claude-opus-4-8-20260530', { serviceTier: 'fast' }));

  // 8. Cost vectors over both sources.
  cases.push(lookupCase('cost:claude-opus-4-6', 'claude-opus-4-6', { usage: [1e6, 1e6, 1e6, 1e6] }));
  cases.push(lookupCase('cost:claude-fable-5', 'claude-fable-5', { usage: [1e6, 1e6, 1e6, 1e6] }));
  cases.push(lookupCase('cost:claude-sonnet-5', 'claude-sonnet-5', { usage: [1e6, 1e6, 1e6, 1e6] }));
  cases.push(lookupCase('cost:claude-sonnet-4-5-mixed', 'claude-sonnet-4-5', { usage: [5e5, 1e5, 5e4, 2e5] }));
  cases.push(lookupCase('cost:gemini-2.0-flash-exp', 'gemini-2.0-flash-exp', { usage: [1e6, 5e5, 1e5, 5e4] }));
  cases.push(lookupCase('cost:zero-usage', 'claude-sonnet-4-5', { usage: [0, 0, 0, 0] }));
  cases.push(lookupCase('cost:unknown', 'unknown-model-xyz', { usage: [1e6, 1e6, 1e6, 1e6] }));
  cases.push(lookupCase('cost:opus-48-fast', 'claude-opus-4-8', { serviceTier: 'fast', usage: [1e6, 1e6, 1e6, 1e6] }));
  cases.push(lookupCase('cost:models-dev-routed', 'gpt-5.5', { fixture: 'main', provider: 'openai', usage: [1e6, 1e6, 1e6, 1e6] }));

  return cases;
}

function buildReads(): ReadCase[] {
  const ttl = 24 * 60 * 60 * 1000;
  const fetchedAt = 1000;
  const written = () => {
    setCachedModelsDevRegistry(MAIN_FIXTURE as never, fetchedAt);
    return fs.readFileSync(path.join(ccsDir(), 'models-dev-registry-cache.json'), 'utf8');
  };
  const cases: ReadCase[] = [];
  const check = (name: string, fileText: string, now: number, allowStale: boolean) => {
    clearModelsDevRegistryCache();
    fs.mkdirSync(ccsDir(), { recursive: true });
    fs.writeFileSync(path.join(ccsDir(), 'models-dev-registry-cache.json'), fileText);
    cases.push({
      name,
      fileText,
      now,
      allowStale,
      reads: getCachedModelsDevRegistry({ allowStale, now }) !== null,
    });
  };
  const fresh = written();
  check('fresh within ttl', fresh, fetchedAt + ttl, false);
  check('exactly at ttl is fresh', fresh, fetchedAt + ttl, false);
  check('just past ttl is stale', fresh, fetchedAt + ttl + 1, false);
  check('past ttl allowed stale', fresh, fetchedAt + ttl + 1, true);
  check('malformed json', '{not json', fetchedAt, true);
  check('wrong version', JSON.stringify({ version: 2, fetchedAt, providers: MAIN_FIXTURE }), fetchedAt, true);
  check('string version', JSON.stringify({ version: '1', fetchedAt, providers: MAIN_FIXTURE }), fetchedAt, true);
  check('missing fetchedAt', JSON.stringify({ version: 1, providers: MAIN_FIXTURE }), fetchedAt, true);
  check('infinite fetchedAt never goes stale', JSON.stringify({ version: 1, fetchedAt: 1e999, providers: MAIN_FIXTURE }), 1e12, false);
  check('empty providers', JSON.stringify({ version: 1, fetchedAt, providers: {} }), fetchedAt, true);
  check('providers of null', JSON.stringify({ version: 1, fetchedAt, providers: null }), fetchedAt, true);
  check('scalar payload', '42', fetchedAt, true);
  return cases;
}

async function buildRefreshes(): Promise<RefreshCase[]> {
  const goodBody = JSON.stringify({
    openai: {
      id: 'openai',
      name: 'OpenAI',
      models: {
        'null-entry': null,
        'string-entry': 'bad',
        'gpt-5.5': { id: 'gpt-5.5', cost: { input: 5, output: 30 } },
      },
    },
    ignored: { id: 'ignored' },
  });
  const cases: RefreshCase[] = [];

  const run = async (name: string, steps: Array<{ fetch: FetchScript; force: boolean; now: number }>) => {
    clearModelsDevRegistryCache();
    let fetches = 0;
    const recorded: RefreshStep[] = [];
    for (const step of steps) {
      let fetchImpl: typeof fetch;
      if (step.fetch.kind === 'transport-error') {
        fetchImpl = async () => {
          fetches += 1;
          throw new Error('offline');
        };
      } else {
        fetchImpl = async () => {
          fetches += 1;
          return new Response(step.fetch.kind === 'ok' ? (step.fetch as { body: string }).body : '{}', {
            status: step.fetch.kind === 'ok' ? 200 : 503,
          });
        };
      }
      const registry = await refreshModelsDevRegistry({ force: step.force, fetchImpl, now: () => step.now });
      const file = path.join(ccsDir(), 'models-dev-registry-cache.json');
      recorded.push({
        fetch: step.fetch,
        force: step.force,
        now: step.now,
        returned: stringify(registry),
        cacheBytes: fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : 'absent',
        fetchesSoFar: fetches,
      });
    }
    cases.push({ name, steps: recorded });
  };

  await run('fetch ok writes the normalized cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
  ]);
  await run('a second non-forced call serves the fresh cache without fetching', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'transport-error' }, force: false, now: 124 },
  ]);
  await run('force refetches past the fresh cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 456 },
  ]);
  await run('http error keeps the stale cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'http-error', body: '{}' }, force: true, now: 1_000_000_000 },
  ]);
  await run('transport error keeps the stale cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'transport-error' }, force: true, now: 1_000_000_000 },
  ]);
  await run('bad payload keeps the stale cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'ok', body: '{"openai": {"id": "openai"}}' }, force: true, now: 1_000_000_000 },
  ]);
  await run('payload without providers keeps the stale cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'ok', body: '[]' }, force: true, now: 1_000_000_000 },
  ]);
  await run('invalid json keeps the stale cache', [
    { fetch: { kind: 'ok', body: goodBody }, force: true, now: 123 },
    { fetch: { kind: 'ok', body: '{not json' }, force: true, now: 1_000_000_000 },
  ]);
  await run('failures with no cache answer null', [
    { fetch: { kind: 'transport-error' }, force: true, now: 123 },
  ]);
  await run('non-forced with no cache fetches', [
    { fetch: { kind: 'ok', body: goodBody }, force: false, now: 123 },
  ]);
  return cases;
}

function buildFacts(): FactsCase[] {
  const cases: FactsCase[] = [];
  const build = (name: string, fixture: string, models: string[], routes: string[]) => {
    installFixture(fixture, 123);
    const withEmpty = [...routes, ''];
    const table: Record<string, unknown> = {};
    for (const model of models) {
      for (const route of withEmpty) {
        table[`${model}\0${route}`] = defaultAccountAnalyticsPricing(model, route || undefined);
      }
    }
    cases.push({ name, fixture, models, routes, table: stringify(table) });
  };
  build('bridge dictionary over the main fixture', 'main', [
    'claude-opus-4-6', 'gpt-5.5', 'gpt-4o', 'qwen3.8-max', 'k3', 'unknown-x', 'gemini-3-flash-preview',
  ], ['openai', 'github-copilot', 'google', 'alibaba-token-plan', '']);
  build('token-plan routes over the policy fixture', 'token-plan', [
    'qwen3.8-max', 'qwen3.7-plus', 'date-suffix-model',
  ], ['alibaba-token-plan', 'aliased-key']);
  build('ambiguous registry', 'ambiguous', ['shared-model', 'agreed-model', 'claude-opus-4-6'], ['first', 'third']);
  return cases;
}

function buildKnownModels(): { fixture: string; models: string } | null {
  installFixture('main', 123);
  return { fixture: 'main', models: stringify(getKnownModels()) };
}

async function build(): Promise<Corpus> {
  return {
    version: 1,
    fixtures: FIXTURES,
    lookups: buildLookups(),
    reads: buildReads(),
    refreshes: await buildRefreshes(),
    facts: buildFacts(),
    knownModels: buildKnownModels(),
  };
}

// ------------------------------------------------------------------ run

const originalHome = process.env.CCS_HOME;
const originalDir = process.env.CCS_DIR;
ccsHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-pricing-corpus-'));
process.env.CCS_HOME = ccsHome;
delete process.env.CCS_DIR;

let failures = 0;
try {
  const corpus = await build();
  const text = JSON.stringify(corpus, null, 1);
  if (process.argv.includes('--write')) {
    fs.writeFileSync(corpusPath, text);
    console.log(
      `wrote ${corpusPath}: ${corpus.lookups.length} lookups, ${corpus.reads.length} reads, ${corpus.refreshes.length} refreshes, ${corpus.facts.length} facts`
    );
  } else {
    const held = fs.readFileSync(corpusPath, 'utf8');
    if (held !== text) {
      failures += 1;
      console.error('cases.json does not match the TS modules; regenerate with --write');
      const heldCases = JSON.parse(held) as Corpus;
      if (heldCases.lookups.length !== corpus.lookups.length) {
        console.error(`lookup count ${heldCases.lookups.length} -> ${corpus.lookups.length}`);
      }
    } else {
      console.log(`cases.json matches the TS modules (${corpus.lookups.length} lookups)`);
    }
  }
} finally {
  clearModelsDevRegistryCache();
  if (originalHome !== undefined) process.env.CCS_HOME = originalHome;
  else delete process.env.CCS_HOME;
  if (originalDir !== undefined) process.env.CCS_DIR = originalDir;
  else delete process.env.CCS_DIR;
  fs.rmSync(ccsHome, { recursive: true, force: true });
}
process.exit(failures === 0 ? 0 : 1);
