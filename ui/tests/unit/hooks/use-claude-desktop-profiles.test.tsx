import { renderHook, waitFor } from '@testing-library/react';
import { QueryClientProvider } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';
import { createTestQueryClient } from '../../setup/test-utils';
import { useClaudeDesktopProfiles } from '@/hooks/use-claude-desktop-profiles';

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
});
