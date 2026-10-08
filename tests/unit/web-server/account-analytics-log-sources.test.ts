import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { extraActivityRequests } from '../../../src/web-server/services/account-analytics-activity';
import { remoteExtraRoots } from '../../../src/web-server/services/analytics-remote-sources';
import {
  defaultDashboardPreferences,
  writeDashboardPreferences,
  type UsageLogSource,
} from '../../../src/web-server/services/dashboard-preferences';

const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
const originalCcsHome = process.env.CCS_HOME;
let root: string;

function source(overrides: Partial<UsageLogSource> & { id: string }): UsageLogSource {
  return { tool: 'omp', host: 'ubuntu', path: root, ...overrides } as UsageLogSource;
}

function extras(sources: UsageLogSource[], scanned = new Set<string>()) {
  return extraActivityRequests(
    sources,
    { minDate: MIN_DATE, cacheDir: path.join(root, 'cache') },
    root,
    scanned
  );
}

describe('extra usage-log sources', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-log-sources-'));
    process.env.CCS_HOME = root;
  });
  afterEach(() => {
    if (originalCcsHome === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = originalCcsHome;
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('maps every ubuntu tool to its worker request', () => {
    const projects = path.join(root, 'projects');
    const codexHome = path.join(root, 'codex');
    const ompRoot = path.join(root, 'omp');
    const museDir = path.join(root, 'muse');
    const db = path.join(root, 'db.sqlite');
    const jsonlRoot = path.join(root, 'jsonl');
    for (const dir of [projects, path.join(codexHome, 'sessions'), ompRoot, museDir, jsonlRoot])
      fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(db, 'sqlite');
    const requests = extras([
      source({ id: 'a', tool: 'claude-code', path: projects }),
      source({ id: 'b', tool: 'codex', path: codexHome }),
      source({ id: 'c', tool: 'omp', path: ompRoot }),
      source({ id: 'd', tool: 'muse', path: museDir }),
      source({ id: 'e', tool: 'zcode', path: db }),
      source({
        id: 'f',
        tool: 'jsonl',
        path: jsonlRoot,
        fieldMapping: { timestamp: 'ts', inputTokens: 'usage.in' },
      }),
    ]);
    expect(requests.map((entry) => entry.provider)).toEqual([
      'claude',
      'codex',
      'omp',
      'muse',
      'zcode',
      'jsonl',
    ]);
    expect(requests.map((entry) => entry.request.kind)).toEqual([
      'claude',
      'codex',
      'omp',
      'muse',
      'zcode',
      'jsonl',
    ]);
    const jsonl = requests[5].request;
    expect(jsonl.kind === 'jsonl' ? jsonl.mapping : null).toEqual({
      timestamp: 'ts',
      inputTokens: 'usage.in',
    });
  });

  it('skips other hosts, unreadable paths and unmapped generic sources', () => {
    fs.mkdirSync(path.join(root, 'logs'), { recursive: true });
    const requests = extras([
      source({ id: 'a', tool: 'omp', host: 'mac', path: path.join(root, 'logs') }),
      source({ id: 'b', tool: 'omp', path: path.join(root, 'missing') }),
      source({ id: 'c', tool: 'zcode', path: path.join(root, 'logs') }),
      source({ id: 'd', tool: 'jsonl', path: path.join(root, 'logs') }),
      // A Nas1 extra is read on Nas1 by the remote scan, never by the local one.
      source({ id: 'e', tool: 'omp', host: 'nas1', path: path.join(root, 'logs') }),
    ]);
    expect(requests).toEqual([]);
  });

  it('never scans one root twice, except generic sources with different mappings', () => {
    const ompRoot = path.join(root, 'omp');
    const jsonlRoot = path.join(root, 'jsonl');
    fs.mkdirSync(ompRoot, { recursive: true });
    fs.mkdirSync(jsonlRoot, { recursive: true });
    const builtIn = fs.realpathSync(ompRoot);
    const requests = extras(
      [
        source({ id: 'a', tool: 'omp', path: ompRoot }),
        source({ id: 'b', tool: 'omp', path: ompRoot }),
        source({
          id: 'c',
          tool: 'jsonl',
          path: jsonlRoot,
          fieldMapping: { timestamp: 'a' },
        }),
        source({
          id: 'd',
          tool: 'jsonl',
          path: jsonlRoot,
          fieldMapping: { timestamp: 'b' },
        }),
        source({
          id: 'e',
          tool: 'jsonl',
          path: jsonlRoot,
          fieldMapping: { timestamp: 'a' },
        }),
      ],
      new Set([`omp:${builtIn}`])
    );
    // The built-in OMP root claims the extra; the duplicate generic mapping
    // collapses while the two different mappings both scan.
    expect(requests.map((entry) => entry.provider)).toEqual(['jsonl', 'jsonl']);
  });

  it('bounds the extra requests like the built-ins', () => {
    const sources: UsageLogSource[] = [];
    for (let index = 0; index < 40; index++) {
      const dir = path.join(root, `omp-${index}`);
      fs.mkdirSync(dir, { recursive: true });
      sources.push(source({ id: `x-${index}`, tool: 'omp', path: dir }));
    }
    expect(extras(sources).length).toBe(24);
  });
});

describe('remote extra usage-log sources', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-log-sources-'));
    process.env.CCS_HOME = root;
  });
  afterEach(() => {
    if (originalCcsHome === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = originalCcsHome;
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('sends only remotely scanned tools for the target host', () => {
    writeDashboardPreferences(
      {
        ...defaultDashboardPreferences(),
        usageLogSources: [
          { id: 'a', tool: 'omp', host: 'mac', path: '/Users/u/extra-omp' },
          { id: 'b', tool: 'muse', host: 'mac', path: '/Users/u/extra-muse' },
          { id: 'c', tool: 'omp', host: 'windows', path: 'C:\\extra\\omp' },
          // Only the kinds the caller scans travel; Ubuntu extras stay local.
          { id: 'd', tool: 'claude-code', host: 'mac', path: '/Users/u/extra' },
          { id: 'e', tool: 'omp', host: 'ubuntu', path: '/home/u/extra' },
          { id: 'f', tool: 'omp', host: 'nas1', path: '/data/nas1/extra-omp' },
          { id: 'g', tool: 'zcode', host: 'nas1', path: '/data/nas1/zcode/db.sqlite' },
          { id: 'h', tool: 'claude-code', host: 'nas1', path: '/data/nas1/extra-projects' },
        ],
      },
      path.join(root, '.ccs')
    );
    expect(remoteExtraRoots('mac', ['omp', 'muse', 'zcode'])).toEqual({
      omp: ['/Users/u/extra-omp'],
      muse: ['/Users/u/extra-muse'],
    });
    expect(remoteExtraRoots('windows', ['omp'])).toEqual({ omp: ['C:\\extra\\omp'] });
    expect(remoteExtraRoots('mac', ['omp'])).toEqual({ omp: ['/Users/u/extra-omp'] });
    // Nas1 gets its own roots, as POSIX paths, and no other host's roots.
    expect(remoteExtraRoots('nas1', ['omp', 'muse', 'zcode'])).toEqual({
      omp: ['/data/nas1/extra-omp'],
      zcode: ['/data/nas1/zcode/db.sqlite'],
    });
    expect(remoteExtraRoots('nas1', ['claude', 'omp'])).toEqual({
      claude: ['/data/nas1/extra-projects'],
      omp: ['/data/nas1/extra-omp'],
    });
    expect(remoteExtraRoots('nas1', ['muse'])).toEqual({});
  });

  it('scans built-in roots alone without saved preferences', () => {
    expect(remoteExtraRoots('mac', ['omp', 'muse', 'zcode'])).toEqual({});
    expect(remoteExtraRoots('nas1', ['omp', 'muse', 'zcode'])).toEqual({});
  });
});
