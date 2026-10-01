import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@tests/setup/test-utils';
import userEvent from '@testing-library/user-event';
import { HomePage } from '@/pages/home';
import type { DashboardAccount } from '@/hooks/use-accounts-dashboard';

const mocks = vi.hoisted(() => ({
  dashboard: vi.fn(),
  refresh: vi.fn(),
  profiles: vi.fn(),
  open: vi.fn(),
}));
vi.mock('@/hooks/use-accounts-dashboard', () => ({
  useAccountsDashboard: mocks.dashboard,
  useRefreshAccountsDashboard: () => ({ mutate: mocks.refresh, isPending: false }),
}));
vi.mock('@/hooks/use-claude-desktop-profiles', () => ({
  useClaudeDesktopProfiles: mocks.profiles,
  useOpenClaudeDesktopProfile: () => ({ mutate: mocks.open, isPending: false }),
}));
vi.mock('@/components/accounts/codex-home-panel', () => ({
  CodexHomePanel: () => (
    <section>
      <h2>Codex</h2>
      <button>Switch account</button>
    </section>
  ),
}));

const profiles = ['platyr', 'gmail', 'party', 'me'].map((id) => ({
  id,
  email: `${id}@example.test`,
  mac: { launcherName: `Claude ${id}`, canOpen: true },
  windows: { launcherName: `Claude ${id}`, launchUri: `ccs-claude://launch/${id}` },
}));
const claude: DashboardAccount = {
  id: 'claude:gmail',
  provider: 'claude',
  providerLabel: 'Claude',
  label: 'gmail',
  email: 'gmail@example.test',
  plan: 'Max',
  platform: 'mac',
  source: 'Claude on Mac',
  status: 'cached',
  message: null,
  fetchedAt: null,
  sampledAt: null,
  isActive: false,
  windows: [
    {
      key: 'five_hour',
      label: '5-hour',
      usedPercent: 42,
      remainingPercent: 58,
      resetAt: '2026-10-01T12:00:00Z',
      windowMinutes: 300,
      used: null,
      limit: null,
      unit: null,
    },
  ],
  capabilities: {
    codexProfile: null,
    claudeProfileId: 'gmail',
    claudePlatforms: ['mac', 'windows'],
  },
};

beforeEach(() => {
  localStorage.removeItem('ccs-claude-computer');
  mocks.refresh.mockReset();
  mocks.open.mockReset();
  mocks.dashboard.mockReturnValue({
    data: { schemaVersion: 1, updatedAt: '2026-10-01T00:00:00Z', accounts: [claude] },
    isLoading: false,
    isFetching: false,
    error: null,
  });
  mocks.profiles.mockReturnValue({
    data: { profiles },
    isLoading: false,
    error: null,
    refetch: vi.fn(),
  });
});

describe('Accounts home dashboard', () => {
  it('puts native controls, all four Claude profiles and seven usage tools on home', () => {
    render(<HomePage />);
    expect(screen.getByRole('heading', { name: 'Your accounts' })).toBeInTheDocument();
    for (const label of [
      'Codex',
      'Claude',
      'Antigravity CLI',
      'Muse Code',
      'Cursor',
      'Kimi Code',
      'Qwen token plan',
      'Z.ai coding plan',
      'OpenCode Go',
    ])
      expect(screen.getByRole('heading', { name: label })).toBeInTheDocument();
    for (const profile of profiles)
      expect(screen.getByRole('heading', { name: profile.email })).toBeInTheDocument();
    expect(
      screen.queryByRole('link', { name: /Analytics|Logs|Proxy|Providers|Shared data/i })
    ).not.toBeInTheDocument();
    expect(screen.getByText('42% used')).toBeInTheDocument();
    expect(screen.getByText(/^Resets /)).toHaveAttribute('datetime', '2026-10-01T12:00:00Z');
  });
  it('opens the allowlisted Mac profile without requiring quota data', async () => {
    mocks.dashboard.mockReturnValue({
      isLoading: false,
      isFetching: false,
      error: new Error('Network down'),
    });
    render(<HomePage />);
    await userEvent.click(
      screen.getByRole('button', { name: 'Open account party@example.test on Mac' })
    );
    expect(mocks.open).toHaveBeenCalledWith('party', expect.any(Object));
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Saved samples and account controls remain available.'
    );
  });
  it('uses Windows profile links after changing computer, preserving the preference', async () => {
    render(<HomePage />);
    await userEvent.click(screen.getByRole('button', { name: 'Windows' }));
    expect(
      screen.getByRole('link', { name: 'Open account gmail@example.test on Windows' })
    ).toHaveAttribute('href', 'ccs-claude://launch/gmail');
    expect(localStorage.setItem).toHaveBeenCalledWith('ccs-claude-computer', 'windows');
    expect(mocks.dashboard).toHaveBeenLastCalledWith('windows');
  });
  it('requests refreshed provider samples from the main refresh control', async () => {
    render(<HomePage />);
    await userEvent.click(screen.getByRole('button', { name: 'Refresh usage' }));
    expect(mocks.refresh).toHaveBeenCalledOnce();
  });
  it('keeps unreported providers visibly unavailable rather than showing zero', () => {
    render(<HomePage />);
    const kimiCard = screen
      .getByRole('heading', { name: 'Kimi Code' })
      .closest('[data-slot="card"]');
    expect(kimiCard).not.toBeNull();
    expect(
      within(kimiCard as HTMLElement).getByText('Subscription usage unavailable.')
    ).toBeInTheDocument();
    expect(within(kimiCard as HTMLElement).queryByText('0% used')).not.toBeInTheDocument();
    expect(within(kimiCard as HTMLElement).queryByRole('button')).not.toBeInTheDocument();
  });
});
