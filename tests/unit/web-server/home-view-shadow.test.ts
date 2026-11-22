/** R1 Home shadow: the comparison hook, the tri-state gate and the fed Home wiring. */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { Request } from 'express';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';
import {
  homeShadowAnswer,
  runHomeShadow,
  type HomeShadowReport,
} from '../../../src/web-server/services/home-view-shadow';
import { rustGroupMode, rustGroupOn } from '../../../src/web-server/rust-service-groups';
import { feedsOf } from '../../../src/web-server/views/feed-publication';
import { withPassedResetsAt } from '../../../src/web-server/services/account-dashboard-service';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import type { RustService } from '../../../src/web-server/rust-service';
import type { RustViewBytes } from '../../../src/web-server/rust-view-reply';

const NOW = Date.parse('2026-11-22T12:00:00.000Z');

function shadowEntries() {
  return getRecentLogEntries().filter((entry) => entry.event === 'home.view.shadow');
}

/** The hook's own comparison signal; no wall-clock wait. */
function comparisonSignal() {
  return Promise.withResolvers<HomeShadowReport>();
}

/** A plain dashboard the per-response pass marks: reset before now, reading before reset. */
function fixture(): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: '2026-11-22T00:00:00.000Z',
    accounts: [
      {
        id: 'acct-1',
        windows: [
          {
            key: 'five_hour',
            resetAt: '2026-11-21T00:00:00.000Z',
            sampledAt: '2026-11-20T00:00:00.000Z',
          },
        ],
      },
    ],
  } as unknown as AccountDashboard;
}

interface FedCall {
  route: string;
  headers: Record<string, string>;
  view: string;
}

/** The private service stand-in: only the fed Home surface the hook reads. */
function stubService(reply: RustViewBytes | string | null): {
  service: RustService;
  calls: FedCall[];
} {
  const calls: FedCall[] = [];
  const service = {
    mode: 'on',
    feedBoot: (_scope: string) => 'boot-1',
    pushFeed: async (_boot: string, _source: string, generation: string) => generation,
    fedView: async (
      _req: Request,
      route: string,
      headers: Record<string, string>,
      view: string
    ) => {
      calls.push({ route, headers, view });
      return reply;
    },
  } as unknown as RustService;
  return { service, calls };
}

function homeReply(member: unknown): RustViewBytes {
  return {
    bytes: Buffer.from(
      JSON.stringify({
        schemaVersion: 1,
        dashboard: member,
        profiles: [],
        antigravity: {},
        view: null,
      })
    ),
    etag: `W/"${'0'.repeat(64)}"`,
    view: 'home@3',
  };
}

function seedHeldGenerations(service: RustService): void {
  const feeds = feedsOf(service, 'boot-1');
  feeds.held.set('home-opens', { key: 'opens-key', generation: '111' });
  feeds.held.set('home-antigravity', { key: 'antigravity-key', generation: '222' });
}

describe('Home shadow (groups.home)', () => {
  const saved = {
    ccsHome: process.env.CCS_HOME,
    ccsDir: process.env.CCS_DIR,
    home: process.env.AAC_RUST_GROUP_HOME,
  };
  let root = '';

  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-home-shadow-'));
    process.env.CCS_HOME = root;
    delete process.env.CCS_DIR;
    delete process.env.AAC_RUST_GROUP_HOME;
    clearRecentLogEntries();
    invalidateLoggingConfigCache();
    const config = createEmptyUnifiedConfig();
    config.logging = { ...config.logging, enabled: true, level: 'debug', redact: false };
    saveUnifiedConfig(config);
    invalidateLoggingConfigCache();
  });

  afterEach(() => {
    for (const [key, value] of [
      ['CCS_HOME', saved.ccsHome],
      ['CCS_DIR', saved.ccsDir],
      ['AAC_RUST_GROUP_HOME', saved.home],
    ] as const) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    fs.rmSync(root, { recursive: true, force: true });
    clearRecentLogEntries();
    invalidateLoggingConfigCache();
  });

  describe('runHomeShadow', () => {
    it('logs one home.view.shadow entry when the numbers differ', async () => {
      const { promise, resolve } = comparisonSignal();
      const served = await runHomeShadow(
        async () => ({ accounts: 1, total: 5 }),
        async () => ({ accounts: 1, total: 6 }),
        resolve
      );
      const report = await promise;
      expect(served).toEqual({ accounts: 1, total: 6 });
      expect(report.outcome).toBe('different');
      const entries = shadowEntries();
      expect(entries.length).toBe(1);
      expect(entries[0]?.context.outcome).toBe('different');
      expect(entries[0]?.context.members).toEqual(['total']);
    });

    it('logs nothing when the views are equal', async () => {
      const { promise, resolve } = comparisonSignal();
      await runHomeShadow(
        async () => ({ b: 2, a: 1 }),
        async () => ({ a: 1, b: 2 }),
        resolve
      );
      const report = await promise;
      expect(report.outcome).toBe('equal');
      expect(shadowEntries().length).toBe(0);
    });

    it('logs each difference class once', async () => {
      const differ = async () => {
        const { promise, resolve } = comparisonSignal();
        await runHomeShadow(async () => ({ total: 5 }), async () => ({ total: 6 }), resolve);
        await promise;
      };
      await differ();
      expect(shadowEntries().length).toBe(1);
      await differ();
      expect(shadowEntries().length).toBe(1);
      const { promise, resolve } = comparisonSignal();
      await runHomeShadow(async () => ({ accounts: 1 }), async () => ({ accounts: 2 }), resolve);
      await promise;
      expect(shadowEntries().length).toBe(2);
      const equal = comparisonSignal();
      await runHomeShadow(async () => ({ total: 6 }), async () => ({ total: 6 }), equal.resolve);
      await equal.promise;
      expect(shadowEntries().length).toBe(2);
      await differ();
      expect(shadowEntries().length).toBe(3);
    });

    it('serves the reference when the candidate falls back or errors', async () => {
      const fallback = comparisonSignal();
      const served = await runHomeShadow(async () => null, async () => ({ total: 6 }), fallback.resolve);
      expect(served).toEqual({ total: 6 });
      expect((await fallback.promise).outcome).toBe('fallback');
      const error = comparisonSignal();
      await runHomeShadow(
        async () => {
          throw new Error('candidate failed');
        },
        async () => ({ total: 6 }),
        error.resolve
      );
      expect((await error.promise).outcome).toBe('error');
      const referenceFailed = comparisonSignal();
      let threw = false;
      try {
        await runHomeShadow(
          async () => ({ total: 5 }),
          async () => {
            throw new Error('reference failed');
          },
          referenceFailed.resolve
        );
      } catch {
        threw = true;
      }
      expect(threw).toBe(true);
      expect((await referenceFailed.promise).outcome).toBe('reference-failed');
      expect(shadowEntries().length).toBe(0);
    });
  });

  describe('the tri-state gate', () => {
    it('keeps the default on and admits shadow without cutting over', () => {
      expect(rustGroupMode('home')).toBe('on');
      expect(rustGroupOn('home')).toBe(true);
      process.env.AAC_RUST_GROUP_HOME = 'shadow';
      expect(rustGroupMode('home')).toBe('shadow');
      expect(rustGroupOn('home')).toBe(false);
      process.env.AAC_RUST_GROUP_HOME = 'yes';
      expect(rustGroupMode('home')).toBe('off');
      expect(rustGroupOn('home')).toBe(false);
      delete process.env.AAC_RUST_GROUP_HOME;
      const config = createEmptyUnifiedConfig();
      config.logging = { ...config.logging, enabled: true, level: 'debug', redact: false };
      config.rustService = { groups: { home: 'shadow' } };
      saveUnifiedConfig(config);
      expect(rustGroupMode('home')).toBe('shadow');
      expect(rustGroupOn('home')).toBe(false);
      process.env.AAC_RUST_GROUP_HOME = 'on';
      expect(rustGroupMode('home')).toBe('on');
    });
  });

  describe('homeShadowAnswer', () => {
    const input = (service: RustService) => ({
      req: {} as Request,
      service,
      platform: 'mac' as const,
      secure: false,
      dashboard: fixture(),
      now: NOW,
      scope: root,
    });

    it('does not ask Rust while groups.home is on', () => {
      const { service, calls } = stubService(null);
      homeShadowAnswer(input(service));
      expect(calls.length).toBe(0);
      expect(shadowEntries().length).toBe(0);
    });

    it('compares the fed Home dashboard member under shadow', async () => {
      process.env.AAC_RUST_GROUP_HOME = 'shadow';
      const passed = JSON.parse(JSON.stringify(withPassedResetsAt(fixture(), NOW)));
      passed.accounts[0].windows[0].resetPassed = false;
      const { service, calls } = stubService(homeReply(passed));
      seedHeldGenerations(service);
      const { promise, resolve } = comparisonSignal();
      homeShadowAnswer(input(service), resolve);
      const report = await promise;
      expect(report.outcome).toBe('different');
      expect(calls.length).toBe(1);
      expect(calls[0]?.route).toBe('/api/views/home');
      expect(calls[0]?.view).toBe('home@3');
      const feed = JSON.parse(calls[0]?.headers['x-aac-feed'] ?? '{}');
      expect(feed.opens).toBe('111');
      expect(feed.antigravity).toBe('222');
      expect(typeof feed.dashboard).toBe('string');
      expect(feed.now).toBe(NOW);
      const entries = shadowEntries();
      expect(entries.length).toBe(1);
      expect(entries[0]?.context.outcome).toBe('different');
      expect(entries[0]?.context.members).toEqual(['accounts']);
    });

    it('logs nothing when the fed Home dashboard member matches', async () => {
      process.env.AAC_RUST_GROUP_HOME = 'shadow';
      const { service, calls } = stubService(homeReply(withPassedResetsAt(fixture(), NOW)));
      seedHeldGenerations(service);
      const { promise, resolve } = comparisonSignal();
      homeShadowAnswer(input(service), resolve);
      expect((await promise).outcome).toBe('equal');
      expect(calls.length).toBe(1);
      expect(shadowEntries().length).toBe(0);
    });

    it('falls back honestly without the opens and antigravity generations', async () => {
      process.env.AAC_RUST_GROUP_HOME = 'shadow';
      const { service, calls } = stubService(homeReply(withPassedResetsAt(fixture(), NOW)));
      const { promise, resolve } = comparisonSignal();
      homeShadowAnswer(input(service), resolve);
      expect((await promise).outcome).toBe('fallback');
      expect(calls.length).toBe(0);
      expect(shadowEntries().length).toBe(0);
    });
  });
});
