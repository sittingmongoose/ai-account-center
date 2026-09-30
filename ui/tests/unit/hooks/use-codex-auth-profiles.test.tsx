import { act, renderHook, waitFor } from '@testing-library/react';
import { QueryClientProvider } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';
import { createTestQueryClient } from '../../setup/test-utils';
import { useActivateCodexAuthProfile, useCodexAuthProfiles } from '@/hooks/use-codex-auth-profiles';

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
