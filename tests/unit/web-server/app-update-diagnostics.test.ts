import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  MESSAGES,
  UPDATE_APP_LABELS,
  normalizeAppUpdateResults,
  normalizeAppUpdateRow,
  type UpdateAppId,
  type UpdatePlatform,
} from '../../../src/web-server/services/app-update-contract';
import {
  normalizeDiagnostics,
  scrubText,
} from '../../../src/web-server/services/app-update-diagnostics';
import { AppUpdateService } from '../../../src/web-server/services/app-update-service';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

function row(appId: UpdateAppId, fields: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    appId,
    status: 'current',
    messageCode: 'current',
    previousVersion: '1.2.3',
    version: '1.2.3',
    manager: 'native',
    updateAttempted: false,
    restartedProcesses: 0,
    diagnostics: { cause: 'none', phases: { check: 12 } },
    ...fields,
  };
}

// The rows the 2026-11-13 failures now produce, as the helper sends them.
const CURSOR_SIGNIN = row('cursor-cli', {
  status: 'action_required',
  messageCode: 'cursor_signin_needed',
  updateAttempted: true,
  previousVersion: '2026.11.11-b912975',
  version: '2026.11.11-b912975',
  diagnostics: {
    cause: 'auth_required',
    reason: 'http_401',
    command: 'cursor-agent update',
    exitCode: 1,
    phases: { update: 650, verify: 20 },
    owner: {
      manager: 'native',
      executable: '~/AppData/Local/cursor-agent/cursor-agent.ps1',
      onPath: true,
      copies: 1,
      source: 'versions-folder',
    },
    tail: '{"event":"global.main.promiseCatch","level":"ERROR","data":{"message":"Update failed: [unauthenticated] Error"}}',
  },
});
const WRONG_PREFIX = row('t3-acp-adapters', {
  status: 'failed',
  messageCode: 'wrong_prefix',
  updateAttempted: true,
  previousVersion: null,
  version: null,
  manager: 'npm',
  parts: [
    { name: 'muse-acp', previousVersion: '0.21.0', version: '0.21.0' },
    { name: 'zcode-acp-server', previousVersion: '0.84.2', version: '0.84.2' },
  ],
  diagnostics: {
    cause: 'wrong_prefix',
    reason: 'owner_unchanged',
    command: 'npm install --global --prefix /opt/homebrew zcode-acp-server@latest',
    exitCode: 0,
    owner: {
      packages: [
        {
          package: 'zcode-acp-server',
          manager: 'npm',
          prefix: '/opt/homebrew',
          source: 't3-config',
          version: '0.84.2',
        },
      ],
      elsewhere: [
        {
          package: 'zcode-acp-server',
          prefix: '~/.local/opt/node-v26.13.1-darwin-arm64',
          version: '0.84.3',
        },
      ],
    },
  },
});

describe('Update apps diagnostics', () => {
  it('keeps the cause, command, exit code, phases, owner and a scrubbed tail', () => {
    const result = normalizeAppUpdateRow(CURSOR_SIGNIN, 'cursor-cli', 'windows');
    expect(result.status).toBe('action_required');
    expect(result.message).toBe(MESSAGES.cursor_signin_needed);
    expect(result.diagnostics).toEqual({
      cause: 'auth_required',
      reason: 'http_401',
      command: 'cursor-agent update',
      exitCode: 1,
      phases: { update: 650, verify: 20 },
      owner: {
        manager: 'native',
        source: 'versions-folder',
        executable: '~/AppData/Local/cursor-agent/cursor-agent.ps1',
        onPath: true,
        copies: 1,
      },
      tail: '{"event":"global.main.promiseCatch","level":"ERROR","data":{"message":"Update failed: [unauthenticated] Error"}}',
    });
    const adapters = normalizeAppUpdateRow(WRONG_PREFIX, 't3-acp-adapters', 'mac');
    expect(adapters.message).toBe(MESSAGES.wrong_prefix);
    expect(adapters.diagnostics?.owner?.elsewhere).toEqual([
      {
        package: 'zcode-acp-server',
        prefix: '~/.local/opt/node-v26.13.1-darwin-arm64',
        version: '0.84.3',
      },
    ]);
  });

  it('scrubs credentials, people and other users\u2019 folders again at the boundary', () => {
    const tail = [
      'npm error 401 Unauthorized - GET https://registry.npmjs.org/x?token=abc123 user:pass',
      'Authorization: Bearer abcdefghijklmnopqrstuvwxyz',
      '//registry.npmjs.org/:_authToken=npm_abcdefghijklmnopqrstuvwxyz',
      'contact someone@example.com from C:\\Users\\otheruser\\AppData and /Users/someone/.npmrc',
      'key ' + 'A'.repeat(48),
    ].join('\n');
    const kept = normalizeDiagnostics({ cause: 'installer_failed', tail })!.tail!;
    for (const secret of [
      'abc123',
      'abcdefghijklmnopqrstuvwxyz',
      'someone@example.com',
      'otheruser',
      'someone',
      'A'.repeat(48),
    ])
      expect(kept).not.toContain(secret);
    expect(kept).toContain('npm error 401 Unauthorized');
    expect(kept).toContain('C:\\Users\\<user>\\AppData');
    expect(scrubText('zcode-acp-server@0.84.3 installed')).toBe(
      'zcode-acp-server@0.84.3 installed'
    );
  });

  it('drops whole credential headers and npm auth lines at the boundary (fix round 1, B1)', () => {
    const tail = [
      'npm error //registry.example.test/:_auth=dXNlcjpmaXh0dXJl',
      'Cookie: SID=fixtureCookieOne; HSID=fixtureCookieTwo',
      'Set-Cookie: sid=abc123; Path=/; HttpOnly',
      'error: login failed for --password hunter2secret --user bob',
      'secret was wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY end',
      'code=4/0AfJohXabc-def_ghi state=xyz',
      'AKIAIOSFODNN7EXAMPLE',
      String.raw`c:\users\Jane Doe\AppData\Roaming\npm`,
      'ssh: connect to host: someone@macbook-pro',
    ].join('\n');
    const kept = normalizeDiagnostics({ cause: 'installer_failed', tail })!.tail!;
    for (const secret of [
      'dXNlcjpmaXh0dXJl',
      'fixtureCookieOne',
      'fixtureCookieTwo',
      'abc123',
      'hunter2secret',
      'wJalrXUtnFEMI',
      '0AfJohX',
      'AKIAIOSFODNN7EXAMPLE',
      'Doe',
      'someone',
    ])
      expect(kept).not.toContain(secret);
    expect(kept).toContain('npm error //registry.example.test/:_auth=<redacted>');
    const ordinary =
      'npm error code E401 zcode-acp-server@latest @brokkai/muse-acp@0.21.0 node_modules/zcode-acp-server/dist/cli.js';
    expect(scrubText(ordinary)).toBe(ordinary);
  });

  it('scrubs a long unbroken run in linear time (fix round 1, B1)', () => {
    const started = performance.now();
    scrubText('a'.repeat(65536));
    scrubText(('x'.repeat(2000) + '\n').repeat(40));
    expect(performance.now() - started).toBeLessThan(1000);
  });

  it('drops unknown causes, fields and shapes instead of passing them through', () => {
    expect(normalizeDiagnostics({ cause: 'because' })).toBeUndefined();
    expect(
      normalizeDiagnostics({
        cause: 'unknown',
        reason: 'Free text with spaces',
        exitCode: 1.5,
        phases: { 'Bad Name': 1, ok: -1, verify: 10 },
        owner: { manager: 'npm', packages: [{ package: 'other', prefix: '/x' }], extra: 'x' },
        notes: ['fine', 'Not fine'],
        tail: 'boom',
        extra: 'dropped',
      })
    ).toEqual({
      cause: 'unknown',
      phases: { verify: 10 },
      owner: { manager: 'npm' },
      notes: ['fine'],
      tail: 'boom',
    });
    // A successful row never carries a tail.
    expect(normalizeDiagnostics({ cause: 'none', tail: 'noise' })).toEqual({ cause: 'none' });
    // Older helpers send none: the row has no diagnostics rather than invented ones.
    expect(
      normalizeAppUpdateRow({ ...row('omp'), diagnostics: undefined }, 'omp', 'ubuntu').diagnostics
    ).toBeUndefined();
  });

  it('accepts each new code only on its own app and status', () => {
    const cases: Array<[string, UpdateAppId, string, UpdatePlatform, Record<string, unknown>?]> = [
      ['cursor_signin_needed', 'cursor-cli', 'action_required', 'windows'],
      ['policy_blocked', 'cursor-cli', 'action_required', 'alexis'],
      ['cursor_shadowed', 'cursor-cli', 'action_required', 'mac'],
      ['registry_mismatch', 't3-acp-adapters', 'failed', 'ubuntu'],
      ['wrong_prefix', 't3-acp-adapters', 'failed', 'mac'],
      ['prefix_not_writable', 't3-acp-adapters', 'action_required', 'mac'],
      ['t3_session_reply_lost', 'antigravity-acp', 'failed', 'windows'],
      ['t3_absent', 'antigravity-acp', 'action_required', 'mac'],
      ['t3_not_ready', 'antigravity-acp', 'action_required', 'windows'],
      ['t3_restart_pending', 'antigravity-acp', 'action_required', 'ubuntu'],
      [
        'antigravity_acp_signin_hold',
        'antigravity-acp',
        'action_required',
        'nas1',
        { heldVersion: '1.5.8' },
      ],
      ['antigravity_acp_signin_needed', 'antigravity-acp', 'action_required', 'ubuntu'],
      [
        'antigravity_acp_models_stale',
        'antigravity-acp',
        'updated',
        'windows',
        { manager: 'official-download' },
      ],
      ['antigravity_acp_offer_unknown', 'antigravity-acp', 'unknown', 'windows'],
    ];
    for (const [code, appId, status, platform, extra] of cases) {
      const accepted = normalizeAppUpdateRow(
        row(appId, { messageCode: code, status, ...(extra ?? {}) }),
        appId,
        platform
      );
      expect(accepted.message).toBe(MESSAGES[code as keyof typeof MESSAGES]);
      expect(
        normalizeAppUpdateRow(row('omp', { messageCode: code, status }), 'omp', platform).message
      ).toBe(MESSAGES.helper_invalid);
    }
    const hold = normalizeAppUpdateRow(
      row('antigravity-acp', {
        messageCode: 'antigravity_acp_signin_hold',
        status: 'action_required',
        version: '1.4.5',
        heldVersion: '1.5.8',
      }),
      'antigravity-acp',
      'nas1'
    );
    expect(hold.heldVersion).toBe('1.5.8');
    // Policy refusals only come from Windows.
    expect(
      normalizeAppUpdateRow(
        row('cursor-cli', { messageCode: 'policy_blocked', status: 'action_required' }),
        'cursor-cli',
        'mac'
      ).message
    ).toBe(MESSAGES.helper_invalid);
  });

  it('keeps diagnostics in the expanded saved job only, within its own size limit', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-diagnostics-'));
    roots.push(root);
    const longTail = 'x '.repeat(500);
    const payload = (platform: UpdatePlatform) =>
      JSON.stringify({
        results: (Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]).map((appId) =>
          appId === 't3-acp-adapters' && platform === 'mac'
            ? WRONG_PREFIX
            : appId === 'cursor-cli' && platform === 'windows'
              ? CURSOR_SIGNIN
              : row(appId, {
                  diagnostics: {
                    cause: 'none',
                    command: 'fixture ' + appId,
                    phases: { check: 5, update: 7 },
                    tail: longTail,
                  },
                })
        ),
      });
    const service = new AppUpdateService({
      ccsDir: root,
      runHost: async (platform) => payload(platform),
    });
    service.start();
    for (let i = 0; i < 50 && service.getStatus().job?.state === 'running'; i++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    const folder = path.join(root, 'app-updates');
    const legacy = fs.readFileSync(path.join(folder, 'dashboard-job.json'), 'utf8');
    const expanded = fs.readFileSync(path.join(folder, 'dashboard-job-expanded.json'), 'utf8');
    expect(legacy).not.toContain('"diagnostics"');
    expect(legacy).not.toContain(MESSAGES.wrong_prefix);
    expect(legacy).toContain(MESSAGES.update_failed);
    expect(expanded).toContain('"diagnostics"');
    expect(legacy.length).toBeLessThan(64 * 1024);
    const restored = new AppUpdateService({ ccsDir: root }).getStatus().job!;
    const adapters = restored.results.find(
      (result) => result.appId === 't3-acp-adapters' && result.platform === 'mac'
    )!;
    expect(adapters.message).toBe(MESSAGES.wrong_prefix);
    expect(adapters.diagnostics?.cause).toBe('wrong_prefix');
    const cursor = restored.results.find(
      (result) => result.appId === 'cursor-cli' && result.platform === 'windows'
    )!;
    expect(cursor.diagnostics?.reason).toBe('http_401');
  });

  it('restores rows saved with the older Antigravity ACP and session words', () => {
    const old = {
      antigravity_acp_updated:
        'T3 installed and activated its pinned Antigravity ACP runtime. Existing sessions keep their version; new sessions use the update.',
      t3_session_issue_unknown:
        'No T3 session in time; no install started. One session may stay valid up to 15 minutes. Check T3.',
    };
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-diagnostics-'));
    roots.push(root);
    fs.mkdirSync(path.join(root, 'app-updates'));
    fs.writeFileSync(
      path.join(root, 'app-updates/dashboard-job.json'),
      JSON.stringify({
        job: {
          id: 'facdec3b-39ed-4a86-b6da-3394b71e3c79',
          state: 'failed',
          startedAt: '2026-11-13T04:55:01.107Z',
          finishedAt: '2026-11-13T04:58:05.769Z',
          results: [
            {
              appId: 'antigravity-acp',
              appLabel: 'Antigravity ACP (T3)',
              platform: 'mac',
              status: 'updated',
              previousVersion: '1.3.0',
              version: '1.4.5',
              manager: 'official-download',
              message: old.antigravity_acp_updated,
              updateAttempted: true,
              restartedProcesses: 0,
              forcedStops: 0,
              restartTargets: [],
            },
            {
              appId: 'antigravity-acp',
              appLabel: 'Antigravity ACP (T3)',
              platform: 'windows',
              status: 'failed',
              previousVersion: '1.3.0',
              version: '1.3.0',
              manager: 'official-download',
              message: old.t3_session_issue_unknown,
              updateAttempted: false,
              restartedProcesses: 0,
              forcedStops: 0,
              restartTargets: [],
            },
          ],
        },
      })
    );
    const job = new AppUpdateService({ ccsDir: root }).getStatus().job!;
    expect(job.results.map((result) => result.message)).toEqual([
      MESSAGES.antigravity_acp_updated,
      MESSAGES.t3_session_issue_unknown,
    ]);
  });

  it('a whole helper document with diagnostics on every row stays under the per-host limit', () => {
    const rows = (Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]).map((appId) =>
      row(appId, {
        status: 'failed',
        messageCode: 'update_failed',
        diagnostics: {
          cause: 'installer_failed',
          command: 'c'.repeat(160),
          exitCode: 1,
          phases: { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8 },
          owner: {
            packages: [
              {
                package: 'muse-acp',
                manager: 'npm',
                prefix: 'p'.repeat(160),
                source: 'path',
                version: '1.0.0',
              },
            ],
          },
          tail: 'y'.repeat(1200),
        },
      })
    );
    const results = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'mac');
    expect(results.every((result) => result.diagnostics?.cause === 'installer_failed')).toBe(true);
  });
});
