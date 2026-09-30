import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, userEvent } from '@tests/setup/test-utils';
import { ClaudeAccountsPage } from '@/pages/claude-accounts';
import { AppSidebar } from '@/components/layout/app-sidebar';
import { SidebarProvider } from '@/components/ui/sidebar';

const profiles = ['personal', 'work', 'family', 'shared'].map((name) => ({
  email: `${name}@example.test`,
  mac: { launcherName: `Claude ${name}`, launcherPath: `/Applications/Claude ${name}.app` },
  windows:
    name === 'personal'
      ? { launcherName: 'Claude', isDefault: true }
      : {
          launcherName: `Claude ${name}`,
          launcherPath: `C:\\Users\\Example\\Desktop\\Claude ${name}.lnk`,
          startMenuPath: `C:\\Users\\Example\\Start Menu\\Claude ${name}.lnk`,
        },
}));

const { useProfiles } = vi.hoisted(() => ({ useProfiles: vi.fn() }));

vi.mock('@/hooks/use-claude-desktop-profiles', () => ({
  useClaudeDesktopProfiles: useProfiles,
}));
vi.mock('@/hooks/use-cliproxy', () => ({ useCliproxyUpdateCheck: () => ({ data: undefined }) }));

beforeEach(() => {
  useProfiles.mockReturnValue({
    data: { profiles },
    isLoading: false,
    error: null,
    isFetching: false,
    refetch: vi.fn(),
  });
});

describe('ClaudeAccountsPage', () => {
  it('shows all configured identities and Mac launchers with clear desktop switching instructions', () => {
    render(<ClaudeAccountsPage />);

    expect(screen.getByRole('heading', { name: 'Claude accounts' })).toBeInTheDocument();
    expect(
      screen.getByText(
        'Open the account’s launcher on your Mac or Windows computer to switch. Sign in to the matching email if prompted.'
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
  });

  it('shows Windows shortcuts and the ordinary installed app without claiming a live active account', async () => {
    const user = userEvent.setup();
    render(<ClaudeAccountsPage />);
    await user.click(screen.getByRole('tab', { name: 'Windows' }));

    expect(screen.getByRole('tab', { name: 'Windows' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('Open Claude')).toBeInTheDocument();
    expect(screen.getByText('Open the installed Claude app on this computer.')).toBeInTheDocument();
    expect(screen.getByText('Original profile')).toBeInTheDocument();
    expect(screen.getAllByText('Start menu shortcut')).toHaveLength(3);
    expect(screen.getByText(profiles[1].windows.startMenuPath as string)).toBeInTheDocument();
    expect(screen.queryByText(profiles[1].mac.launcherPath)).not.toBeInTheDocument();
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
