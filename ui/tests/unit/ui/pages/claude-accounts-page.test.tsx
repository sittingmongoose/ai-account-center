import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, userEvent, within } from '@tests/setup/test-utils';
import { ClaudeAccountsPage } from '@/pages/claude-accounts';
import { AppSidebar } from '@/components/layout/app-sidebar';
import { SidebarProvider } from '@/components/ui/sidebar';

const profiles = ['personal', 'work', 'family', 'shared'].map((name) => ({
  id: name,
  email: `${name}@example.test`,
  mac: {
    launcherName: `Claude ${name}`,
    launcherPath: `/Applications/Claude ${name}.app`,
    canOpen: true,
  },
  windows:
    name === 'personal'
      ? { launcherName: 'Claude', isDefault: true, launchUri: `ccs-claude://launch/${name}` }
      : {
          launcherName: `Claude ${name}`,
          launcherPath: `C:\\Users\\Example\\Desktop\\Claude ${name}.lnk`,
          startMenuPath: `C:\\Users\\Example\\Start Menu\\Claude ${name}.lnk`,
          launchUri: `ccs-claude://launch/${name}`,
        },
}));

const { useProfiles, useUsage, openProfile } = vi.hoisted(() => ({
  useProfiles: vi.fn(),
  useUsage: vi.fn(),
  openProfile: vi.fn(),
}));

vi.mock('@/hooks/use-claude-desktop-profiles', () => ({
  useClaudeDesktopProfiles: useProfiles,
  useClaudeDesktopUsage: useUsage,
  useOpenClaudeDesktopProfile: () => ({ mutate: openProfile, isPending: false }),
}));
vi.mock('@/hooks/use-cliproxy', () => ({ useCliproxyUpdateCheck: () => ({ data: undefined }) }));

beforeEach(() => {
  openProfile.mockReset();
  useProfiles.mockReturnValue({
    data: { profiles },
    isLoading: false,
    error: null,
    isFetching: false,
    refetch: vi.fn(),
  });
  useUsage.mockReturnValue({
    isLoading: false,
    isFetching: false,
    error: null,
    refetch: vi.fn(),
    data: {
      profiles: [
        {
          id: 'family',
          email: 'family@example.test',
          status: 'needs-sign-in',
          cached: true,
          fetchedAt: '2026-10-01T00:00:00Z',
          sampledAt: null,
          utilization: {},
        },
        {
          id: 'personal',
          email: 'personal@example.test',
          status: 'cached',
          cached: true,
          fetchedAt: '2026-10-01T00:00:00Z',
          sampledAt: '2026-09-30T23:00:00Z',
          utilization: { fiveHour: 0, weekly: 100 },
        },
      ],
    },
  });
});

describe('ClaudeAccountsPage', () => {
  it('shows all configured identities and Mac launchers with clear desktop switching instructions', () => {
    render(<ClaudeAccountsPage />);

    expect(screen.getByRole('heading', { name: 'Claude accounts' })).toBeInTheDocument();
    expect(
      screen.getByText(
        'Choose your computer, then click Open account. Sign in with the matching email if prompted.'
      )
    ).toBeInTheDocument();
    for (const profile of profiles) {
      expect(screen.getByRole('heading', { name: profile.email })).toBeInTheDocument();
      expect(screen.getByText(profile.mac.launcherPath)).toBeInTheDocument();
    }
    expect(screen.getByRole('link', { name: 'Claude CLI profiles' })).toHaveAttribute(
      'href',
      '/accounts'
    );
    expect(screen.queryByRole('button', { name: /Activate|Set default/i })).not.toBeInTheDocument();
    expect(screen.queryByText('Active on VM')).not.toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Open account personal@example.test' })
    ).toBeEnabled();
  });

  it('shows Windows shortcuts and the ordinary installed app without claiming a live active account', async () => {
    const user = userEvent.setup();
    render(<ClaudeAccountsPage />);
    await user.click(screen.getByRole('tab', { name: 'Windows' }));

    expect(screen.getByRole('tab', { name: 'Windows' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('Launcher locations · Claude')).toBeInTheDocument();
    expect(screen.getByText('Open the installed Claude app on this computer.')).toBeInTheDocument();
    expect(screen.getByText('Original profile')).toBeInTheDocument();
    expect(screen.getAllByText('Start menu shortcut')).toHaveLength(3);
    expect(screen.getByText(profiles[1].windows.startMenuPath as string)).toBeInTheDocument();
    expect(screen.queryByText(profiles[1].mac.launcherPath)).not.toBeInTheDocument();
    expect(
      screen.getByRole('link', { name: 'Open account personal@example.test' })
    ).toHaveAttribute('href', 'ccs-claude://launch/personal');
    expect(
      screen.getByText(
        'Use a Windows browser to open these accounts. Your browser may ask you to open the launcher app.'
      )
    ).toBeInTheDocument();
    expect(openProfile).not.toHaveBeenCalled();
  });

  it('keeps configured accounts visible even when a computer has no launcher for an account', async () => {
    useProfiles.mockReturnValue({
      data: { profiles: [{ email: 'mac-only@example.test', mac: profiles[0].mac }] },
      isLoading: false,
      error: null,
      refetch: vi.fn(),
    });
    const user = userEvent.setup();
    render(<ClaudeAccountsPage />);
    await user.click(screen.getByRole('tab', { name: 'Windows' }));

    expect(screen.getByRole('heading', { name: 'mac-only@example.test' })).toBeInTheDocument();
    expect(screen.getByText('No launcher configured for Windows.')).toBeInTheDocument();
  });

  it('copies a launcher location', async () => {
    const user = userEvent.setup();
    const clipboard = vi.spyOn(navigator.clipboard, 'writeText');
    render(<ClaudeAccountsPage />);
    await user.click(screen.getAllByRole('button', { name: 'Copy Launcher location' })[0]);

    expect(clipboard).toHaveBeenCalledWith(profiles[0].mac.launcherPath);
  });

  it('opens only the selected Mac profile without claiming a signed-in account', async () => {
    const user = userEvent.setup();
    render(<ClaudeAccountsPage />);
    await user.click(screen.getByRole('button', { name: 'Open account work@example.test' }));
    expect(openProfile).toHaveBeenCalledTimes(1);
    expect(openProfile).toHaveBeenCalledWith(
      'work',
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) })
    );
    expect(screen.queryByText(/Activated .*@/)).not.toBeInTheDocument();
  });

  it('associates cached percentages with each profile and preserves missing usage as unknown', () => {
    render(<ClaudeAccountsPage />);
    const personal = screen
      .getByRole('heading', { name: 'personal@example.test' })
      .closest('[data-slot="card"]');
    const family = screen
      .getByRole('heading', { name: 'family@example.test' })
      .closest('[data-slot="card"]');
    const work = screen
      .getByRole('heading', { name: 'work@example.test' })
      .closest('[data-slot="card"]');
    if (!personal || !family || !work) throw new Error('Account cards missing');
    expect(within(personal as HTMLElement).getByText('0% used')).toBeInTheDocument();
    expect(within(personal as HTMLElement).getByText('100% used')).toBeInTheDocument();
    expect(within(personal as HTMLElement).getByText(/Recorded /)).toBeInTheDocument();
    expect(
      within(family as HTMLElement).getByText(
        'No cached usage yet; open this account and sign in if prompted.'
      )
    ).toBeInTheDocument();
    expect(within(family as HTMLElement).queryByRole('progressbar')).not.toBeInTheDocument();
    expect(
      within(work as HTMLElement).getByText('Subscription usage unavailable.')
    ).toBeInTheDocument();
    expect(within(work as HTMLElement).queryByText('0% used')).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Recorded CLI usage' })).toHaveAttribute(
      'href',
      '/analytics'
    );
  });

  it('keeps launch controls usable when quota retrieval fails', () => {
    useUsage.mockReturnValue({
      error: new Error('Remote usage unavailable'),
      isLoading: false,
      refetch: vi.fn(),
    });
    render(<ClaudeAccountsPage />);
    expect(screen.getAllByText('Unable to load subscription usage.')).toHaveLength(4);
    expect(screen.getByRole('button', { name: 'Open account work@example.test' })).toBeEnabled();
  });

  it.each([
    [{ isLoading: true }, 'Loading Claude accounts…'],
    [
      { error: new Error('Private metadata validation failed') },
      'Unable to load Claude account launchers. Try refreshing.',
    ],
    [
      { data: { profiles: [] } },
      'No desktop account launchers are configured for this CCS instance.',
    ],
  ])('handles unavailable metadata honestly', (state, message) => {
    useProfiles.mockReturnValue({ ...useProfiles(), ...state });
    render(<ClaudeAccountsPage />);
    expect(screen.getByText(message)).toBeInTheDocument();
    expect(screen.queryByText('Private metadata validation failed')).not.toBeInTheDocument();
  });

  it('provides a direct Claude accounts sidebar link beside Codex accounts', () => {
    render(
      <SidebarProvider>
        <AppSidebar />
      </SidebarProvider>
    );

    expect(screen.getByRole('link', { name: 'Claude accounts' })).toHaveAttribute(
      'href',
      '/claude/accounts'
    );
    expect(screen.getByRole('link', { name: 'Codex accounts' })).toHaveAttribute(
      'href',
      '/codex/accounts'
    );
  });
});
