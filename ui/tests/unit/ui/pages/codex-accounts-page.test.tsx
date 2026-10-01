import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@tests/setup/test-utils';
import userEvent from '@testing-library/user-event';
import { CodexAccountsPage } from '@/pages/codex-accounts';

const diagnostics = vi.hoisted(() =>
  vi.fn(() => {
    throw new Error('Codex diagnostics are unavailable');
  })
);
const automaticSwitch = vi.hoisted(() => vi.fn());
const updateAutomaticSwitch = vi.hoisted(() => vi.fn());
const saveAutomaticSwitch = vi.hoisted(() => vi.fn());
const retryAutomaticSwitch = vi.hoisted(() => vi.fn());

vi.mock('@/hooks/use-codex', () => ({ useCodex: diagnostics }));
vi.mock('@/hooks/use-codex-auth-profiles', () => ({
  useCodexAuthProfiles: () => ({
    isLoading: false,
    error: null,
    data: {
      active: null,
      default: 'personal',
      activated: { name: 'personal', email: 'personal@example.test', plan: 'pro' },
      profiles: ['personal', 'work', 'family'].map((name) => ({
        name,
        email: `${name}@example.test`,
        plan: 'pro',
        authValid: true,
        lastUsed: null,
      })),
    },
  }),
  useActivateCodexAuthProfile: () => ({ mutate: vi.fn(), isPending: false }),
  useCodexAuthProfileQuotas: () => ({ data: { profiles: [] }, isLoading: false, error: null }),
  useCodexAutomaticSwitch: automaticSwitch,
  useUpdateCodexAutomaticSwitch: updateAutomaticSwitch,
}));

const disabledSwitchStatus = {
  enabled: false,
  thresholdPercent: 5,
  pollIntervalSeconds: 60,
  outcome: 'disabled',
  message: 'Automatic switching is disabled.',
  activationInProgress: false,
};

beforeEach(() => {
  saveAutomaticSwitch.mockReset();
  retryAutomaticSwitch.mockReset();
  automaticSwitch.mockReturnValue({
    data: disabledSwitchStatus,
    isLoading: false,
    error: null,
    refetch: retryAutomaticSwitch,
  });
  updateAutomaticSwitch.mockReturnValue({
    mutate: saveAutomaticSwitch,
    isPending: false,
    error: null,
  });
});

describe('CodexAccountsPage', () => {
  it('shows all saved accounts and switch actions independently of Codex diagnostics', () => {
    render(<CodexAccountsPage />);

    expect(screen.getByRole('heading', { name: 'Codex accounts' })).toBeInTheDocument();
    expect(
      screen.getByText(
        'Click Activate beside the account you want to use. Usage windows and reset times are shown as reported by each account. Percentages show how much is used.'
      )
    ).toBeInTheDocument();
    expect(screen.getByText('work@example.test')).toBeInTheDocument();
    expect(screen.getByText('family@example.test')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Activate personal' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Activate work' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Activate family' })).toBeEnabled();
    expect(diagnostics).not.toHaveBeenCalled();
  });

  it('links recorded CLI usage separately from subscription limits', () => {
    render(<CodexAccountsPage />);
    expect(screen.getByRole('link', { name: 'Recorded CLI usage' })).toHaveAttribute(
      'href',
      '/analytics'
    );
  });

  it('requests enabling without claiming the setting is enabled before the server confirms it', async () => {
    render(<CodexAccountsPage />);
    const control = screen.getByRole('switch', { name: 'Automatic switching' });
    expect(control).not.toBeChecked();
    await userEvent.setup().click(control);
    expect(saveAutomaticSwitch).toHaveBeenCalledExactlyOnceWith(true);
    expect(control).not.toBeChecked();
    expect(screen.getByText('Claude account switching stays manual.')).toBeInTheDocument();
    expect(
      screen.getByText(
        'Switch to a healthy saved account when 5% or less remains. Waits until Codex is idle before restarting the VM desktop and app server.'
      )
    ).toBeInTheDocument();
  });

  it('keeps the control disabled and unchecked until settings load', () => {
    automaticSwitch.mockReturnValue({ data: undefined, isLoading: true, error: null });
    render(<CodexAccountsPage />);
    const control = screen.getByRole('switch', { name: 'Automatic switching' });
    expect(control).toBeDisabled();
    expect(control).not.toBeChecked();
    expect(screen.getByText('Loading automatic switching…')).toBeInTheDocument();
    expect(screen.queryByText('Enabled')).not.toBeInTheDocument();
  });

  it('shows a load error and offers retry without allowing a guessed toggle', async () => {
    automaticSwitch.mockReturnValue({
      data: undefined,
      isLoading: false,
      error: new Error('Unavailable'),
      refetch: retryAutomaticSwitch,
    });
    render(<CodexAccountsPage />);
    expect(screen.getByRole('switch', { name: 'Automatic switching' })).toBeDisabled();
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Unable to load automatic switching settings.'
    );
    await userEvent.setup().click(screen.getByRole('button', { name: 'Retry' }));
    expect(retryAutomaticSwitch).toHaveBeenCalledOnce();
  });

  it('preserves enabled state while saving and explains that a busy Codex waits until idle', () => {
    automaticSwitch.mockReturnValue({
      data: { ...disabledSwitchStatus, enabled: true, outcome: 'waiting_idle' },
      isLoading: false,
      error: null,
    });
    updateAutomaticSwitch.mockReturnValue({
      mutate: saveAutomaticSwitch,
      isPending: true,
      error: null,
    });
    render(<CodexAccountsPage />);
    const control = screen.getByRole('switch', { name: 'Automatic switching' });
    expect(control).toBeChecked();
    expect(control).toBeDisabled();
    expect(screen.getByRole('status')).toHaveTextContent(
      'Codex is busy. Waiting until it is idle before switching.'
    );
  });

  it('keeps the saved setting and reports a failed toggle', () => {
    automaticSwitch.mockReturnValue({
      data: { ...disabledSwitchStatus, enabled: true, outcome: 'healthy' },
      isLoading: false,
      error: null,
    });
    updateAutomaticSwitch.mockReturnValue({
      mutate: saveAutomaticSwitch,
      isPending: false,
      error: new Error('Settings could not be written.'),
    });
    render(<CodexAccountsPage />);
    expect(screen.getByRole('switch', { name: 'Automatic switching' })).toBeChecked();
    expect(screen.getByRole('alert')).toHaveTextContent('Settings could not be written.');
    expect(screen.getByRole('status')).toHaveTextContent('The current account has enough quota.');
  });
});
