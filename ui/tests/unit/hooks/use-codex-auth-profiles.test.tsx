import { act, renderHook, waitFor } from '@testing-library/react';
import { QueryClientProvider } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';
import { createTestQueryClient } from '../../setup/test-utils';
import {
  useActivateCodexAuthProfile,
  useCodexAuthProfiles,
  useCodexAuthProfileQuotas,
  useCodexAutomaticSwitch,
  useUpdateCodexAutomaticSwitch,
} from '@/hooks/use-codex-auth-profiles';

function response(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const summary = {
  active: { name: 'personal', source: 'default', codexHome: '/fake/profiles/personal' },
  default: 'personal',
  activated: {
    name: 'personal',
    email: 'personal@example.test',
    plan: 'pro',
    codexHome: '/fake/.codex',
  },
  profiles: [],
};

function createWrapper() {
  const queryClient = createTestQueryClient();
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
}

describe('Codex auth activation hooks', () => {
  it('posts the selected account once and refreshes actual activated state without changing the launch default', async () => {
    const activated = {
      name: 'work',
      email: 'work@example.test',
      plan: 'pro',
      codexHome: '/fake/.codex',
    };
    let switched = false;
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'POST') {
        switched = true;
        return Promise.resolve(
          response({ success: true, ...activated, previousEmail: summary.activated.email })
        );
      }
      return Promise.resolve(
        response({ ...summary, activated: switched ? activated : summary.activated })
      );
    });
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(
      () => ({ profiles: useCodexAuthProfiles(), activation: useActivateCodexAuthProfile() }),
      { wrapper: createWrapper() }
    );
    await waitFor(() => expect(result.current.profiles.data?.activated?.name).toBe('personal'));
    await act(async () => {
      await result.current.activation.mutateAsync('work');
    });
    await waitFor(() => expect(result.current.profiles.data?.activated?.name).toBe('work'));
    expect(result.current.profiles.data?.default).toBe('personal');
    expect(fetchMock.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1);
    expect(fetchMock).toHaveBeenCalledWith('/api/codex/profiles/work/activate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
    });
    expect(fetchMock.mock.calls.filter(([, init]) => !init?.method)).toHaveLength(2);
  });

  it('shows safe server errors and refreshes state after a failed activation', async () => {
    const fetchMock = vi.fn((_input: RequestInfo | URL, init?: RequestInit) =>
      Promise.resolve(
        init?.method === 'POST'
          ? response({ code: 'busy', error: 'Codex is busy. Try again when work finishes.' }, 409)
          : response(summary)
      )
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(
      () => ({ profiles: useCodexAuthProfiles(), activation: useActivateCodexAuthProfile() }),
      { wrapper: createWrapper() }
    );
    await waitFor(() => expect(result.current.profiles.data).toBeDefined());
    await act(async () => {
      await expect(result.current.activation.mutateAsync('work')).rejects.toThrow('Codex is busy.');
    });
    await waitFor(() =>
      expect(result.current.activation.error?.message).toContain('when work finishes')
    );
    expect(fetchMock.mock.calls.filter(([, init]) => !init?.method)).toHaveLength(2);
  });
});

const disabledSwitchStatus = {
  enabled: false,
  thresholdPercent: 5,
  pollIntervalSeconds: 60,
  outcome: 'disabled',
  message: 'Automatic switching is disabled.',
  activationInProgress: false,
};

describe('Codex automatic switching hooks', () => {
  it('loads the saved setting and backend outcome from its dedicated endpoint', async () => {
    const status = { ...disabledSwitchStatus, enabled: true, outcome: 'waiting_idle' };
    const fetchMock = vi.fn(() => Promise.resolve(response(status)));
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useCodexAutomaticSwitch(), { wrapper: createWrapper() });
    await waitFor(() => expect(result.current.data).toEqual(status));
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith('/api/codex/profiles/auto-switch');
  });

  it('sends only the enabled flag and updates the cache after the server responds', async () => {
    const savedStatus = { ...disabledSwitchStatus, enabled: true, outcome: 'scheduled' };
    let finishSave!: (res: Response) => void;
    const saved = new Promise<Response>((resolve) => {
      finishSave = resolve;
    });
    const fetchMock = vi.fn((_input: RequestInfo | URL, init?: RequestInit) =>
      init?.method === 'PUT' ? saved : Promise.resolve(response(disabledSwitchStatus))
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(
      () => ({ settings: useCodexAutomaticSwitch(), update: useUpdateCodexAutomaticSwitch() }),
      { wrapper: createWrapper() }
    );
    await waitFor(() => expect(result.current.settings.data?.enabled).toBe(false));
    act(() => result.current.update.mutate(true));
    await waitFor(() => expect(result.current.update.isPending).toBe(true));
    expect(result.current.settings.data?.enabled).toBe(false);
    expect(fetchMock).toHaveBeenCalledWith('/api/codex/profiles/auto-switch', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: '{"enabled":true}',
    });
    await act(async () => finishSave(response(savedStatus)));
    await waitFor(() => expect(result.current.settings.data).toEqual(savedStatus));
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('preserves saved enabled state when disabling fails', async () => {
    const currentStatus = { ...disabledSwitchStatus, enabled: true, outcome: 'healthy' };
    const fetchMock = vi.fn((_input: RequestInfo | URL, init?: RequestInit) =>
      Promise.resolve(
        init?.method === 'PUT'
          ? response({ error: 'Settings could not be written.' }, 500)
          : response(currentStatus)
      )
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(
      () => ({ settings: useCodexAutomaticSwitch(), update: useUpdateCodexAutomaticSwitch() }),
      { wrapper: createWrapper() }
    );
    await waitFor(() => expect(result.current.settings.data?.enabled).toBe(true));
    await act(async () => {
      await expect(result.current.update.mutateAsync(false)).rejects.toThrow(
        'Settings could not be written.'
      );
    });
    expect(result.current.settings.data).toEqual(currentStatus);
    expect(fetchMock).toHaveBeenCalledWith('/api/codex/profiles/auto-switch', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: '{"enabled":false}',
    });
  });

  it('does not let an older poll overwrite the confirmed saved setting', async () => {
    const savedStatus = { ...disabledSwitchStatus, enabled: true, outcome: 'scheduled' };
    let finishPoll!: (res: Response) => void;
    const pendingPoll = new Promise<Response>((resolve) => {
      finishPoll = resolve;
    });
    let reads = 0;
    const fetchMock = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === 'PUT') return Promise.resolve(response(savedStatus));
      reads += 1;
      return reads === 1 ? Promise.resolve(response(disabledSwitchStatus)) : pendingPoll;
    });
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(
      () => ({ settings: useCodexAutomaticSwitch(), update: useUpdateCodexAutomaticSwitch() }),
      { wrapper: createWrapper() }
    );
    await waitFor(() => expect(result.current.settings.data?.enabled).toBe(false));
    act(() => {
      void result.current.settings.refetch();
    });
    await waitFor(() => expect(reads).toBe(2));
    await act(async () => {
      await result.current.update.mutateAsync(true);
    });
    await waitFor(() => expect(result.current.settings.data).toEqual(savedStatus));
    await act(async () => finishPoll(response(disabledSwitchStatus)));
    expect(result.current.settings.data).toEqual(savedStatus);
  });
});

describe('Codex subscription quota hook', () => {
  it('loads dedicated quota data independently of account activation and retains unread statuses', async () => {
    const quotas = {
      profiles: [
        { profileName: 'work', status: 'reauth_required', windows: [] },
        {
          profileName: 'personal',
          status: 'available',
          windows: [{ key: 'primary', label: '5-hour limit', usedPercent: 7 }],
        },
        { profileName: 'unsigned', status: 'not_connected', windows: [] },
      ],
    };
    const fetchMock = vi.fn(() => Promise.resolve(response(quotas)));
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useCodexAuthProfileQuotas(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(result.current.data).toEqual(quotas));
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith('/api/codex/profiles/quotas');
  });

  it('returns an error after one retry without inventing a zero-usage response', async () => {
    const fetchMock = vi.fn(() => Promise.resolve(response({ error: 'Unavailable' }, 503)));
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useCodexAuthProfileQuotas(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(result.current.isError).toBe(true), { timeout: 5000 });
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(result.current.data).toBeUndefined();
    expect(result.current.error?.message).toBe('Failed to fetch Codex subscription usage');
  });
});
