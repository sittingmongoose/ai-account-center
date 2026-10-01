import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@tests/setup/test-utils';
import userEvent from '@testing-library/user-event';
import { CodexHomePanel } from '@/components/accounts/codex-home-panel';

const hooks = vi.hoisted(() => ({
  profiles: vi.fn(),
  quotas: vi.fn(),
  activation: vi.fn(),
  automaticSwitch: vi.fn(),
  updateAutomaticSwitch: vi.fn(),
  activate: vi.fn(),
  saveAutomaticSwitch: vi.fn(),
  retryAutomaticSwitch: vi.fn(),
}));

vi.mock('@/hooks/use-codex-auth-profiles', () => ({
  useCodexAuthProfiles: hooks.profiles,
  useCodexAuthProfileQuotas: hooks.quotas,
  useActivateCodexAuthProfile: hooks.activation,
  useCodexAutomaticSwitch: hooks.automaticSwitch,
  useUpdateCodexAutomaticSwitch: hooks.updateAutomaticSwitch,
}));

const profileData = {
  active: null,
  default: 'work',
  activated: {
    name: 'personal',
    email: 'personal@example.test',
    plan: 'pro',
    codexHome: '/home/test/.codex',
  },
  profiles: ['personal', 'work', 'family'].map((name) => ({
    name,
    email: `${name}@example.test`,
    plan: 'pro',
    codexHome: `/home/test/.ccs/codex/${name}`,
    accountId: null,
    authValid: true,
    lastUsed: null,
  })),
};

const enabledSwitchStatus = {
  enabled: true,
  thresholdPercent: 5,
  pollIntervalSeconds: 60,
  outcome: 'healthy',
  message: 'The current account has enough quota.',
  activationInProgress: false,
};

beforeEach(() => {
  vi.clearAllMocks();
  hooks.profiles.mockReturnValue({ data: profileData, isLoading: false, error: null });
  hooks.quotas.mockReturnValue({ data: { profiles: [] }, isLoading: false, error: null });
  hooks.activation.mockReturnValue({ mutate: hooks.activate, isPending: false, error: null });
  hooks.automaticSwitch.mockReturnValue({
    data: enabledSwitchStatus,
    isLoading: false,
    error: null,
    refetch: hooks.retryAutomaticSwitch,
  });
  hooks.updateAutomaticSwitch.mockReturnValue({
    mutate: hooks.saveAutomaticSwitch,
    isPending: false,
    error: null,
  });
});

describe('CodexHomePanel', () => {
  it('shows the saved accounts and disables the live account instead of the CCS default', () => {
    render(<CodexHomePanel />);

    expect(screen.getByRole('heading', { name: 'Codex', level: 2 })).toBeInTheDocument();
    expect(screen.getByText('personal@example.test')).toBeInTheDocument();
    expect(screen.getByText('work@example.test')).toBeInTheDocument();
    expect(screen.getByText('family@example.test')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Activate personal' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Activate work' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Activate family' })).toBeEnabled();
  });

  it('forwards the selected profile name to the activation mutation', async () => {
    render(<CodexHomePanel />);

    await userEvent.setup().click(screen.getByRole('button', { name: 'Activate work' }));

    expect(hooks.activate).toHaveBeenCalledOnce();
    expect(hooks.activate.mock.calls[0][0]).toBe('work');
  });

  it('shows reported quota and its reset time beside the matching account', () => {
    hooks.quotas.mockReturnValue({
      data: {
        profiles: [
          {
            profileName: 'personal',
            status: 'available',
            windows: [
              {
                key: 'five_hour',
                label: 'Primary',
                usedPercent: 35.5,
                resetsAt: '2026-10-01T05:00:00Z',
              },
            ],
          },
        ],
      },
      isLoading: false,
      error: null,
    });
    render(<CodexHomePanel />);

    const personalAccount = within(screen.getByRole('article', { name: 'personal Codex account' }));
    expect(personalAccount.getByText('35.5% used')).toBeInTheDocument();
    expect(personalAccount.getByRole('progressbar', { name: '5-hour limit' })).toHaveAttribute(
      'aria-valuenow',
      '35.5'
    );
    expect(personalAccount.getByText(/Resets /)).toBeInTheDocument();
    expect(
      within(screen.getByRole('article', { name: 'work Codex account' })).queryByRole('progressbar')
    ).not.toBeInTheDocument();
  });

  it('does not turn malformed or nonfinite quota into zero usage', () => {
    hooks.quotas.mockReturnValue({
      data: {
        profiles: [
          {
            profileName: 'personal',
            status: 'available',
            windows: [
              { key: 'five_hour', label: 'Primary', usedPercent: Number.NaN },
              { key: 'seven_day', label: 'Weekly', usedPercent: Number.POSITIVE_INFINITY },
              { key: 'extra', label: 'Extra usage', usedPercent: 'unknown' },
            ],
          },
        ],
      },
      isLoading: false,
      error: null,
    });
    render(<CodexHomePanel />);

    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    expect(screen.queryByText('0% used')).not.toBeInTheDocument();
    expect(screen.getAllByText('Subscription usage unavailable.')).toHaveLength(3);
  });

  it('handles missing windows and null window entries without inventing quota', () => {
    hooks.quotas.mockReturnValue({
      data: {
        profiles: [
          { profileName: 'personal', status: 'available' },
          { profileName: 'work', status: 'available', windows: [null] },
        ],
      },
      isLoading: false,
      error: null,
    });
    render(<CodexHomePanel />);

    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    expect(screen.queryByText('0% used')).not.toBeInTheDocument();
    expect(screen.getAllByText('Subscription usage unavailable.')).toHaveLength(3);
  });

  it('requests disabling without changing the server-confirmed enabled state', async () => {
    render(<CodexHomePanel />);
    const control = screen.getByRole('switch', { name: 'Automatic switching' });

    expect(control).toBeChecked();
    await userEvent.setup().click(control);

    expect(hooks.saveAutomaticSwitch).toHaveBeenCalledExactlyOnceWith(false);
    expect(control).toBeChecked();
  });

  it('keeps automatic switching enabled and locked while a save is pending', () => {
    hooks.updateAutomaticSwitch.mockReturnValue({
      mutate: hooks.saveAutomaticSwitch,
      isPending: true,
      error: null,
    });
    render(<CodexHomePanel />);
    const control = screen.getByRole('switch', { name: 'Automatic switching' });

    expect(control).toBeChecked();
    expect(control).toBeDisabled();
  });

  it('preserves the confirmed setting after a failed save and reports the error', () => {
    hooks.updateAutomaticSwitch.mockReturnValue({
      mutate: hooks.saveAutomaticSwitch,
      isPending: false,
      error: new Error('Settings could not be written.'),
    });
    render(<CodexHomePanel />);

    expect(screen.getByRole('switch', { name: 'Automatic switching' })).toBeChecked();
    expect(screen.getByRole('alert')).toHaveTextContent('Settings could not be written.');
  });

  it('leaves automatic switching disabled and unchecked until settings load', () => {
    hooks.automaticSwitch.mockReturnValue({ data: undefined, isLoading: true, error: null });
    render(<CodexHomePanel />);
    const control = screen.getByRole('switch', { name: 'Automatic switching' });

    expect(control).not.toBeChecked();
    expect(control).toBeDisabled();
    expect(screen.getByText('Loading automatic switching…')).toBeInTheDocument();
  });

  it('offers a settings retry without letting a load failure guess the setting', async () => {
    hooks.automaticSwitch.mockReturnValue({
      data: undefined,
      isLoading: false,
      error: new Error('Unavailable'),
      refetch: hooks.retryAutomaticSwitch,
    });
    render(<CodexHomePanel />);

    expect(screen.getByRole('switch', { name: 'Automatic switching' })).toBeDisabled();
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Unable to load automatic switching settings.'
    );
    await userEvent.setup().click(screen.getByRole('button', { name: 'Retry' }));
    expect(hooks.retryAutomaticSwitch).toHaveBeenCalledOnce();
  });

  it('omits removal, launch-default, and analytics controls from the home panel', () => {
    render(<CodexHomePanel />);

    expect(
      screen.queryByRole('button', { name: /remove|delete|default/i })
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('link', { name: /analytics|recorded cli usage/i })
    ).not.toBeInTheDocument();
  });
});
