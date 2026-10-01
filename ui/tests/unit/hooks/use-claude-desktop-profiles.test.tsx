import { act, renderHook, waitFor } from '@testing-library/react';
import { QueryClientProvider } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';
import { createTestQueryClient } from '../../setup/test-utils';
import {
  useClaudeDesktopProfiles,
  useClaudeDesktopUsage,
  useOpenClaudeDesktopProfile,
} from '@/hooks/use-claude-desktop-profiles';

function createWrapper() {
  const queryClient = createTestQueryClient();
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
}

describe('Claude desktop profiles hook', () => {
  it('reads only configured launcher metadata from the authenticated API', async () => {
    const summary = {
      profiles: [
        { email: 'example@example.test', windows: { launcherName: 'Claude', isDefault: true } },
      ],
    };
    const fetchMock = vi.fn(() =>
      Promise.resolve(new Response(JSON.stringify(summary), { status: 200 }))
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useClaudeDesktopProfiles(), { wrapper: createWrapper() });
    await waitFor(() => expect(result.current.data).toEqual(summary));

    expect(fetchMock).toHaveBeenCalledWith('/api/claude/desktop-profiles');
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('reports failed requests without exposing the response body', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(new Response('Private manifest detail', { status: 500 })))
    );
    const { result } = renderHook(() => useClaudeDesktopProfiles(), { wrapper: createWrapper() });
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.message).toBe('Failed to load Claude desktop profiles');
    expect(result.current.data).toBeUndefined();
  });

  it('posts one allowlisted Mac account id without changing authentication state', async () => {
    const fetchMock = vi.fn(() =>
      Promise.resolve(
        new Response(JSON.stringify({ opened: true, id: 'work', platform: 'mac' }), { status: 200 })
      )
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useOpenClaudeDesktopProfile(), {
      wrapper: createWrapper(),
    });
    await act(async () => {
      await result.current.mutateAsync('work');
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith('/api/claude/desktop-profiles/work/open', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ platform: 'mac' }),
    });
    await waitFor(() =>
      expect(result.current.data).toEqual({ opened: true, id: 'work', platform: 'mac' })
    );
  });

  it('reports sanitized launcher failures without retrying the launch', async () => {
    const fetchMock = vi.fn(() =>
      Promise.resolve(
        new Response(JSON.stringify({ error: 'Remote launcher could not be opened.' }), {
          status: 502,
        })
      )
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useOpenClaudeDesktopProfile(), {
      wrapper: createWrapper(),
    });
    await act(async () => {
      await expect(result.current.mutateAsync('work')).rejects.toThrow(
        'Remote launcher could not be opened.'
      );
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('queries the selected platform cache without loading historical CLI analytics', async () => {
    const summary = { platform: 'windows', fetchedAt: '2026-10-01T00:00:00Z', profiles: [] };
    const fetchMock = vi.fn(() =>
      Promise.resolve(new Response(JSON.stringify(summary), { status: 200 }))
    );
    vi.stubGlobal('fetch', fetchMock);
    const { result } = renderHook(() => useClaudeDesktopUsage('windows'), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(result.current.data).toEqual(summary));
    expect(fetchMock).toHaveBeenCalledWith('/api/claude/desktop-profiles/usage?platform=windows');
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
