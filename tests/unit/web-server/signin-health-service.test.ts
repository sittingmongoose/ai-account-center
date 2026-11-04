import { describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';
import {
  allowProfile,
  cacheTtlFor,
  collectorFailureWord,
  downgradeCrossedRows,
  publishableError,
  publishableFailureReason,
  hasPassedDeadline as hasPassedDeadlineWindow,
  nextDeadlineAt,
  parseRows,
  sanitizeName,
  type SignInHealth,
} from '../../../src/web-server/services/signin-health-service';

/** Pure helpers of the sign-in health service: the publish allowlist, the
 * per-host row parsing, and the deadline-aware cache lifetime. The runners
 * themselves are thin execFile wrappers exercised in production; the remote
 * machines run the same collector through the usage helper's stdin bundle. */

const row = (overrides: Record<string, unknown> = {}) => ({
  name: 'me',
  provider: 'claude',
  configDir: '~/.claude',
  accessExpiresAt: '2026-11-04T09:43:00Z',
  refreshExpiresAt: '2026-11-30T23:04:00Z',
  loggedOutAt: null,
  state: 'ok',
  fixCommand: 'CLAUDE_CONFIG_DIR="$HOME/.claude" claude auth login',
  ...overrides,
});

const health = (rows: Array<Record<string, unknown>>): SignInHealth => ({
  generatedAt: '2026-11-03T12:00:00Z',
  machines: { ubuntu: rows as never },
});

describe('allowProfile', () => {
  it('projects a valid row with the machine on platform and machine', () => {
    const out = allowProfile(row(), 'mac');
    expect(out?.machine).toBe('mac');
    expect(out?.platform).toBe('mac');
    expect(out?.state).toBe('ok');
    expect(out?.configDir).toBe('~/.claude');
    expect(out?.provider).toBe('claude');
    expect(allowProfile(row({ provider: 'codex' }), 'mac')?.provider).toBe('codex');
    expect(allowProfile(row({ provider: { junk: true } }), 'mac')?.provider).toBe('claude');
  });

  it('keeps a row whose name the strict pattern refuses, sanitized', () => {
    const email = allowProfile(row({ name: 'person@mail.com' }), 'mac');
    expect(email?.name).toBe('person@mail.com');
    const apostrophe = allowProfile(row({ name: "it's" }), 'mac');
    expect(apostrophe).not.toBeNull();
    expect(apostrophe?.name).not.toContain("'");
    expect(apostrophe?.name.length).toBeGreaterThan(0);
    expect(allowProfile(row({ name: 'bad name!' }), 'mac')).not.toBeNull();
    expect(allowProfile(row({ name: '' }), 'mac')?.name).toBe('unnamed');
    expect(allowProfile(row({ name: 42 }), 'mac')?.name).toBe('unnamed');
    expect(sanitizeName('a'.repeat(80))).toHaveLength(64);
    expect(sanitizeName('   ')).toBe('unnamed');
  });

  it('never drops a row over a bound; paths clip and overlong commands are omitted whole', () => {
    const clipped = allowProfile(
      row({ configDir: 'x'.repeat(300), fixCommand: `${'claude auth login; '.repeat(30)}` }),
      'mac'
    );
    expect(clipped).not.toBeNull();
    expect(clipped?.configDir).toHaveLength(200);
    // A command truncated mid-quote is worse than none: overlong omits whole.
    expect(clipped?.fixCommand).toBe('');
    const longButFitting = allowProfile(row({ fixCommand: 'x'.repeat(200) }), 'mac');
    expect(longButFitting?.fixCommand).toHaveLength(200);
    // The scoped Windows form (selector clearing plus the USERPROFILE
    // fallback expression) is long before the account suffix; the bound
    // leaves room for a real one instead of dropping the command.
    const atBound = allowProfile(row({ fixCommand: 'x'.repeat(400) }), 'mac');
    expect(atBound?.fixCommand).toHaveLength(400);
    expect(allowProfile(row({ fixCommand: 'x'.repeat(401) }), 'mac')?.fixCommand).toBe('');
  });

  it('omits a repair command that cannot render byte-exact, never rewrites it', () => {
    // A newline inside a quoted path is part of the path: folding it to a
    // space would aim the command at a different directory.
    const out = allowProfile(row({ fixCommand: 'claude auth login\nsecond line' }), 'mac');
    expect(out?.fixCommand).toBe('');
    expect(out).not.toBeNull();
    // A backtick cannot live in a markdown code span without changing what
    // the command does: such a command is omitted whole, never rewritten.
    const backticked = allowProfile(
      row({ fixCommand: '$env:CLAUDE_CONFIG_DIR = "$HOME\`$dir"; claude auth login' }),
      'mac'
    );
    expect(backticked?.fixCommand).toBe('');
    const clean = allowProfile(row({ fixCommand: 'claude auth login' }), 'mac');
    expect(clean?.fixCommand).toBe('claude auth login');
    // C1 controls are controls too: U+0085 omits the command whole.
    const c1 = allowProfile(row({ fixCommand: 'claude auth\u0085login' }), 'mac');
    expect(c1?.fixCommand).toBe('');
  });

  it('omits a repair command whose path cannot survive UTF-8 publication', () => {
    // A non-UTF-8 directory name reaches JSON as a lone surrogate; writing
    // it to markdown would replace it with U+FFFD and aim the pasted
    // command at a different directory, so it is omitted whole. A valid
    // surrogate pair is ordinary text and passes.
    const lone = allowProfile(
      row({
        fixCommand: 'CLAUDE_CONFIG_DIR="$HOME/.claude-t3/bad\udcff" claude auth login',
        configDir: '~/.claude-t3/bad\udcff',
      }),
      'ubuntu'
    );
    expect(lone?.fixCommand).toBe('');
    expect(lone?.configDir).toBeUndefined();
    const paired = allowProfile(
      row({ fixCommand: 'CLAUDE_CONFIG_DIR="$HOME/.claude-t3/\u{1F600}" claude auth login' }),
      'ubuntu'
    );
    expect(paired?.fixCommand).toContain('\u{1F600}');
  });

  it('omits a repair command carrying bidi controls', () => {
    const out = allowProfile(row({ fixCommand: 'claude auth\u202elogin' }), 'mac');
    expect(out?.fixCommand).toBe('');
  });

  it('publishes only the collector fixed error literals, nothing else', () => {
    for (const literal of [
      'profile directory resolves outside the allowed roots',
      'credentials file missing or unreadable',
      'expiry times unreadable',
      'keychain item unreadable',
      'keychain item holds no readable credentials',
      'auth file missing or unreadable',
      "profile discovery was incomplete; this machine's list may be missing homes",
      'sign-in state changed during collection; recheck pending',
    ])
      expect(publishableError(literal)).toBe(literal);
    // Every unlisted shape collapses: quoted and separator-joined
    // absolute paths, relative paths, addresses, credential shapes.
    for (const other of [
      "open '/home/user/.claude/.credentials.json' failed",
      'path=/home/user/private',
      'failed:/home/user/private',
      'home/user/.claude is gone',
      'write to user@mac.example.internal failed',
      'token ' + 'sk' + '-abcdefgh rejected',
      'some future wording',
    ])
      expect(publishableError(other)).toBe('unavailable');
    expect(publishableError('')).toBeNull();
    expect(publishableError(42)).toBeNull();
  });

  it('keeps the allowlist tied to the collector error literals', () => {
    // The Python collector's fixed error texts are the allowlist's source
    // of truth: wording edited on one side must not silently collapse to
    // the fixed word on the other.
    const collector = fs.readFileSync(
      path.join(
        path.dirname(fileURLToPath(import.meta.url)),
        '../../../scripts/account-usage/signin_expiry.py'
      ),
      'utf8'
    );
    const literals = new Set<string>();
    // Both quote styles, dict/assignment/keyword forms: a future literal
    // in any of today's plausible spellings must be seen here, not slip
    // past because the extraction only knew one shape.
    for (const match of collector.matchAll(
      /(?:\[(?:"error"|'error')\]\s*=|(?:"error"|'error')\s*:|\berror\s*=)\s*(?:"([^"]+)"|'([^']+)')/g
    ))
      literals.add(match[1] ?? match[2]);
    expect(literals.size).toBe(7);
    for (const literal of literals) expect(publishableError(literal)).toBe(literal);
  });

  it('screens control characters out of configDir and error', () => {
    const out = allowProfile(
      row({ configDir: '~/bad\u0000dir', state: 'unavailable', error: 'bad\u001b[31m news' }),
      'mac'
    );
    expect(out?.configDir).toBeUndefined();
    // An unlisted error publishes as the fixed word, never cleaned text.
    expect(out?.error).toBe('unavailable');
  });

  it('omits a repair command or configDir carrying bidi marks', () => {
    for (const mark of ['\u061c', '\u200e', '\u200f', '\ufeff']) {
      const out = allowProfile(
        row({ fixCommand: `claude auth${mark}login`, configDir: `~/.claude-t3/a${mark}b` }),
        'mac'
      );
      expect(out?.fixCommand).toBe('');
      expect(out?.configDir).toBeUndefined();
    }
  });

  it('reads malformed stamps as absent, never as a reason to drop the row', () => {
    const out = allowProfile(
      row({ accessExpiresAt: 'not-a-time', refreshExpiresAt: 'x'.repeat(41) }),
      'mac'
    );
    expect(out).not.toBeNull();
    expect(out?.accessExpiresAt).toBeNull();
    expect(out?.refreshExpiresAt).toBeNull();
    expect(allowProfile(row({ accessExpiresAt: 12345 }), 'mac')?.accessExpiresAt).toBeNull();
  });

  it('rejects impossible calendar dates in every offset form', () => {
    for (const bad of [
      '2026-02-30T00:00:00Z',
      '2026-02-30T00:00:00+00:00',
      '2026-13-01T00:00:00-05:00',
    ])
      expect(allowProfile(row({ accessExpiresAt: bad }), 'mac')?.accessExpiresAt).toBeNull();
    // A real date whose UTC day differs from its wall day still publishes.
    expect(
      allowProfile(row({ accessExpiresAt: '2026-11-04T01:30:00+05:30' }), 'mac')?.accessExpiresAt
    ).toBe('2026-11-04T01:30:00+05:30');
  });

  it('never passes helper-added fields through to the published DTO', () => {
    const out = allowProfile(row({ accessToken: 'sk-ant-secret', extra: 'field' }), 'ubuntu');
    expect(out).not.toBeNull();
    expect(JSON.stringify(out)).not.toContain('sk-ant-secret');
    expect(JSON.stringify(out)).not.toContain('extra');
  });

  it('carries an error only on unavailable rows, as a fixed literal or the fixed word', () => {
    const failed = allowProfile(
      row({ state: 'unavailable', error: 'ssh: refused\n\n   connection timed out   ' }),
      'ubuntu'
    );
    // Unlisted text is never cleaned and republished; it collapses whole.
    expect(failed?.error).toBe('unavailable');
    expect(allowProfile(row({ error: 'some error' }), 'ubuntu')?.error).toBeUndefined();
    expect(
      allowProfile(
        row({ state: 'unavailable', error: 'credentials file missing or unreadable' }),
        'ubuntu'
      )?.error
    ).toBe('credentials file missing or unreadable');
    // Host-level failure words reach rows through unavailableHost, never
    // through a collector row: as a row error they are unlisted text.
    expect(
      allowProfile(row({ state: 'unavailable', error: 'connection refused' }), 'ubuntu')?.error
    ).toBe('unavailable');
  });

  it('caps fieldsAvailable on unsupported rows', () => {
    const out = allowProfile(
      row({
        provider: 'codex',
        state: 'unsupported',
        fixCommand: '',
        fieldsAvailable: [...Array(40).keys()].map((n) => `field_${n}`),
      }),
      'ubuntu'
    );
    expect(out?.fieldsAvailable).toHaveLength(32);
    const noFields = allowProfile(row({ state: 'unsupported', fieldsAvailable: 'nope' }), 'ubuntu');
    expect(noFields?.fieldsAvailable).toBeUndefined();
  });
});

describe('parseRows', () => {
  const envelope = (profiles: unknown[]) =>
    JSON.stringify({ schemaVersion: 1, generatedAt: '2026-11-03T12:00:00Z', profiles });

  it('parses the collector envelope and names the machine on every row', () => {
    const rows = parseRows(envelope([row(), row({ name: 'party' })]), 'windows');
    expect(rows).toHaveLength(2);
    expect(rows.every((r) => r.machine === 'windows' && r.platform === 'windows')).toBe(true);
  });

  it('reads banner-prefixed or unreadable output as nothing', () => {
    expect(parseRows(`banner line\n${envelope([])}`, 'mac')).toEqual([]);
    expect(parseRows('{"schemaVersion":2,"profiles":[]}', 'mac')).toEqual([]);
    expect(parseRows('not json at all', 'mac')).toEqual([]);
  });
});

describe('the cache outlives no deadline it published', () => {
  const NOW = Date.parse('2026-11-03T12:00:00Z');

  it('finds the earliest future stamp across machines', () => {
    const report = health([
      row({ refreshExpiresAt: '2026-11-03T12:04:00Z' }),
      row({ name: 'party', accessExpiresAt: '2026-11-03T12:02:00Z' }),
    ]);
    expect(nextDeadlineAt(report, NOW)).toBe(Date.parse('2026-11-03T12:02:00Z'));
    expect(nextDeadlineAt(health([]), NOW)).toBeNull();
    expect(
      nextDeadlineAt(
        health([row({ accessExpiresAt: '2026-11-03T11:00:00Z', refreshExpiresAt: null })]),
        NOW
      )
    ).toBeNull();
  });

  it('only deadlines crossing after collection invalidate, never historical stamps', () => {
    // An idle login's expired access token and a logged-out row's end are
    // facts about the past: they must not force a recollection every read.
    const historical = health([
      row({ accessExpiresAt: '2026-11-03T02:00:00Z', state: 'ok' }),
      row({ name: 'gone', state: 'logged_out', loggedOutAt: '2026-11-03T01:00:00Z' }),
    ]);
    expect(hasPassedDeadlineWindow(historical, NOW - 3 * 3600_000, NOW)).toBe(false);
    // A deadline that crosses after collection does invalidate.
    const crossing = health([row({ refreshExpiresAt: '2026-11-03T12:05:00Z' })]);
    expect(hasPassedDeadlineWindow(crossing, NOW, NOW + 10 * 60_000)).toBe(true);
    expect(hasPassedDeadlineWindow(crossing, NOW - 60_000, NOW + 6 * 60_000)).toBe(true);
    expect(hasPassedDeadlineWindow(crossing, NOW + 6 * 60_000, NOW + 60_000)).toBe(false);
    // A terminal row whose end crossed mid-collection was already observed
    // as terminal: it must not force a recollection on the next read.
    const terminal = health([row({ state: 'logged_out', loggedOutAt: '2026-11-03T12:05:00Z' })]);
    expect(hasPassedDeadlineWindow(terminal, NOW, NOW + 10 * 60_000)).toBe(false);
  });

  it('clamps the cache lifetime to the next deadline, with a floor and a ceiling', () => {
    const twoMinutes = health([row({ refreshExpiresAt: '2026-11-03T12:02:00Z' })]);
    expect(cacheTtlFor(twoMinutes, NOW)).toBe(2 * 60_000);
    // A deadline ten seconds away must not pin every publish to a miss.
    const tenSeconds = health([row({ refreshExpiresAt: '2026-11-03T12:00:10Z' })]);
    expect(cacheTtlFor(tenSeconds, NOW)).toBe(30_000);
    // Far deadlines and no deadlines use the full five minutes.
    const far = health([row({ refreshExpiresAt: '2026-11-30T23:04:00Z' })]);
    expect(cacheTtlFor(far, NOW)).toBe(5 * 60_000);
    expect(cacheTtlFor(health([]), NOW)).toBe(5 * 60_000);
  });

  it('downgrades only live rows a deadline crossed during collection', () => {
    const report = health([
      row({ name: 'crossed', refreshExpiresAt: '2026-11-03T12:05:00Z' }),
      row({ name: 'stable', refreshExpiresAt: '2026-11-30T23:04:00Z' }),
      row({ name: 'gone', state: 'logged_out', loggedOutAt: '2026-11-03T12:05:00Z' }),
      row({ name: 'past', accessExpiresAt: '2026-11-03T02:00:00Z', state: 'ok' }),
    ]);
    expect(downgradeCrossedRows(report, NOW, NOW + 10 * 60_000)).toBe(true);
    const [crossed, stable, gone, past] = report.machines.ubuntu;
    expect(crossed.state).toBe('unavailable');
    expect(crossed.refreshExpiresAt).toBeNull();
    expect(crossed.error).toContain('recheck pending');
    expect(stable.state).toBe('ok');
    // Terminal rows and stamps already past at collection stand as facts.
    expect(gone.state).toBe('logged_out');
    expect(past.state).toBe('ok');
    expect(downgradeCrossedRows(report, NOW, NOW + 10 * 60_000)).toBe(false);
  });

  it('treats a live row with a past refresh end and no named end as crossed', () => {
    // No collector on one clock emits this shape — a past refresh end
    // with no derivable login end reads logged_out at the source — so a
    // live row carrying one comes from a clock that disagrees with
    // this one, exactly like a past loggedOutAt. A past ACCESS stamp on
    // a renewable login (refresh still ahead) stays a historical fact,
    // and a dying window (refresh past, named end still ahead) stands.
    const skewed = health([
      row({ name: 'skewed', state: 'warn', loggedOutAt: null, refreshExpiresAt: '2026-11-03T11:00:00Z' }),
      row({ name: 'renewable', accessExpiresAt: '2026-11-03T11:00:00Z', loggedOutAt: null }),
      row({
        name: 'dying',
        state: 'critical',
        refreshExpiresAt: '2026-11-03T11:00:00Z',
        loggedOutAt: '2026-11-03T14:00:00Z',
      }),
    ]);
    expect(hasPassedDeadlineWindow(skewed, NOW, NOW)).toBe(true);
    expect(downgradeCrossedRows(skewed, NOW, NOW)).toBe(true);
    const [skewedRow, renewable, dying] = skewed.machines.ubuntu;
    expect(skewedRow.state).toBe('unavailable');
    expect(renewable.state).toBe('ok');
    expect(dying.state).toBe('critical');
  });

  it('treats a live row whose known end is already past as crossed, clock skew included', () => {
    // A remote whose clock runs behind can report a live state whose
    // loggedOutAt is already past here. That known end cannot be a live
    // future fact, unlike an expired access stamp on a renewable login.
    const skewed = health([
      row({ name: 'skewed', state: 'critical', loggedOutAt: '2026-11-03T11:59:00Z' }),
      row({ name: 'renewable', accessExpiresAt: '2026-11-03T11:00:00Z', loggedOutAt: null }),
    ]);
    expect(hasPassedDeadlineWindow(skewed, NOW, NOW)).toBe(true);
    expect(downgradeCrossedRows(skewed, NOW, NOW)).toBe(true);
    const [skewedRow, renewable] = skewed.machines.ubuntu;
    expect(skewedRow.state).toBe('unavailable');
    expect(renewable.state).toBe('ok');
  });
});

describe('collectorFailureWord', () => {
  it('publishes only allowlisted words, never routing details from stderr', () => {
    const cases: Array<[string, string]> = [
      ['ssh: connect to host 10.0.0.9 port 22: Connection timed out', 'collection timed out'],
      ['ssh: connect to host mac.lan port 22: Connection refused', 'connection refused'],
      ['someone@10.0.0.9: Permission denied (publickey).', 'machine sign-in refused'],
      [
        'ssh: Could not resolve hostname secret-alias: Name or service not known',
        'host unreachable',
      ],
      ['python3: /home/user/.ccs/secret.py crashed', 'collector failed'],
      ['', 'collector failed'],
    ];
    for (const [stderr, word] of cases) {
      const published = collectorFailureWord(stderr);
      expect(published).toBe(word);
      expect(published).not.toContain('10.0.0.9');
      expect(published).not.toContain('someone');
      expect(published).not.toContain('secret');
    }
  });

  it('admits only the fixed words at the publication boundary', () => {
    expect(publishableFailureReason('host unreachable')).toBe('host unreachable');
    expect(publishableFailureReason('collection timed out')).toBe('collection timed out');
    // A setup failure (bundle read, spawn) quotes paths and targets: it
    // collapses to the fixed word instead of publishing.
    const leaked = publishableFailureReason(
      "ENOENT: no such file or directory, open '/home/user/install/signin_expiry.py'"
    );
    expect(leaked).toBe('collector failed');
    expect(publishableFailureReason('ssh secret-alias exited 255')).toBe('collector failed');
    expect(publishableFailureReason(new Error('boom'))).toBe('collector failed');
    expect(publishableFailureReason(undefined)).toBe('collector failed');
  });
});
