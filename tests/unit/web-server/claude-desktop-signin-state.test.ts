import { afterEach, describe, expect, it } from 'bun:test';
import {
  getClaudeMacSignInState,
  invalidateClaudeMacSignInStateCache,
} from '../../../src/web-server/services/claude-desktop-signin-state';
import type { ClaudeHostTransport } from '../../../src/web-server/services/claude-host-transport';
import type { ClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-profile-service';

const fake: ClaudeDesktopProfile = {
  id: 'fake-one',
  email: 'fake-one@example.com',
  mac: {
    launcherName: 'Claude (fake-one)',
    launcherPath: '/Users/fixture/Applications/Claude (fake-one).app',
    profilePath: '/Users/fixture/Library/Application Support/Claude-fake-one',
    sshHost: 'fixture-mac',
  },
};

function transport(answers: Array<'signed-in' | 'signed-out' | Error>) {
  const calls: unknown[] = [];
  const value = {
    sessionState: async (host: string, input: unknown) => {
      calls.push([host, input]);
      const next = answers.shift() ?? 'signed-in';
      if (next instanceof Error) throw next;
      return next;
    },
  } as unknown as ClaudeHostTransport;
  return { value, calls };
}

afterEach(() => invalidateClaudeMacSignInStateCache());

describe('Mac Claude sign-in state before Open', () => {
  it('asks the fixed session step for the profile folder only and caches the answer', async () => {
    let clock = 1_000_000;
    const t = transport(['signed-out', 'signed-in']);
    const options = { transport: t.value, now: () => clock };
    expect(await getClaudeMacSignInState(fake, options)).toBe('signed-out');
    expect(t.calls).toEqual([
      [
        'mac',
        {
          launcher: {
            launcherName: 'Claude (fake-one)',
            profilePath: '/Users/fixture/Library/Application Support/Claude-fake-one',
            sshHost: 'fixture-mac',
          },
        },
      ],
    ]);
    clock += 60_000;
    expect(await getClaudeMacSignInState(fake, options)).toBe('signed-out');
    expect(t.calls.length).toBe(1);
    // A forced refresh re-checks once the answer is at least 30 s old.
    expect(await getClaudeMacSignInState(fake, { ...options, refresh: true })).toBe('signed-in');
    expect(t.calls.length).toBe(2);
  });

  it('answers unknown (null) when the Mac cannot be reached, and retries after a minute', async () => {
    let clock = 5_000_000;
    const t = transport([new Error('ssh: connect timed out'), 'signed-in']);
    const options = { transport: t.value, now: () => clock };
    expect(await getClaudeMacSignInState(fake, options)).toBeNull();
    clock += 30_000;
    expect(await getClaudeMacSignInState(fake, options)).toBeNull();
    clock += 31_000;
    expect(await getClaudeMacSignInState(fake, options)).toBe('signed-in');
    expect(t.calls.length).toBe(2);
  });

  it('never runs without a Mac profile folder and ssh alias', async () => {
    const t = transport([]);
    for (const profile of [
      { id: 'fake-two', email: 'x@example.com' },
      { ...fake, mac: { ...fake.mac!, profilePath: undefined } },
      { ...fake, mac: { ...fake.mac!, sshHost: undefined } },
      { ...fake, mac: { ...fake.mac!, profilePath: 'relative/Claude' } },
    ] as ClaudeDesktopProfile[])
      expect(await getClaudeMacSignInState(profile, { transport: t.value })).toBeNull();
    expect(t.calls.length).toBe(0);
  });
});
