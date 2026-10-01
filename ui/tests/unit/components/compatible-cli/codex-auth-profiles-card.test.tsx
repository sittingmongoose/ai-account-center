import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, userEvent, within } from '@tests/setup/test-utils';
import { CodexAuthProfilesCard } from '@/components/compatible-cli/codex-auth-profiles-card';
import type { CodexAuthProfilesResponse } from '@/hooks/use-codex-auth-profiles';

const hooks = vi.hoisted(() => ({ profiles: vi.fn(), activation: vi.fn(), quotas: vi.fn() }));
vi.mock('@/hooks/use-codex-auth-profiles', () => ({
  useCodexAuthProfiles: hooks.profiles,
  useActivateCodexAuthProfile: hooks.activation,
  useCodexAuthProfileQuotas: hooks.quotas,
}));

const summary: CodexAuthProfilesResponse = {
  active: { name: 'personal', source: 'default', codexHome: '/fake/profiles/personal' },
  default: 'personal',
  activated: { name: 'work', email: 'work@example.test', plan: 'pro', codexHome: '/fake/.codex' },
  profiles: ['personal', 'work', 'unsigned'].map((name) => ({
    name,
    codexHome: `/fake/profiles/${name}`,
    email: name === 'unsigned' ? null : `${name}@example.test`,
    plan: 'pro',
    accountId: null,
    lastUsed: null,
    authValid: name !== 'unsigned',
  })),
};

function mutation(overrides = {}) {
  return {
    mutate: vi.fn(),
    isPending: false,
    isSuccess: false,
    error: null,
    variables: undefined,
    data: undefined,
    ...overrides,
  };
}

beforeEach(() => {
  hooks.profiles.mockReturnValue({ data: summary, isLoading: false, error: null });
  hooks.activation.mockReturnValue(mutation());
  hooks.quotas.mockReturnValue({ data: { profiles: [] }, isLoading: false, error: null });
});

describe('CodexAuthProfilesCard activation', () => {
  it('marks the live VM account separately from the CCS launch default', () => {
    render(<CodexAuthProfilesCard />);
    expect(screen.getByText('VM Codex account (~/.codex)')).toBeInTheDocument();
    expect(screen.getByText('CCS launch default: personal')).toBeInTheDocument();
    const activeRow = screen.getByText('Active on VM').closest('tr');
    if (!activeRow) throw new Error('Missing activated profile row');
    expect(within(activeRow).getByText('work@example.test')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Activate personal' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Activate work' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Activate unsigned' })).toBeDisabled();
  });

  it('activates a valid profile with one click', async () => {
    const activate = mutation();
    hooks.activation.mockReturnValue(activate);
    render(<CodexAuthProfilesCard />);
    await userEvent.click(screen.getByRole('button', { name: 'Activate personal' }));
    expect(activate.mutate).toHaveBeenCalledOnce();
    expect(activate.mutate).toHaveBeenCalledWith('personal');
  });

  it('disables structurally valid profiles without an account email', () => {
    hooks.profiles.mockReturnValue({
      data: {
        ...summary,
        profiles: [{ ...summary.profiles[0], name: 'sparse', authValid: true, email: null }],
      },
      isLoading: false,
      error: null,
    });
    render(<CodexAuthProfilesCard />);
    const button = screen.getByRole('button', { name: 'Activate sparse' });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute(
      'title',
      'Activation needs a saved login with an account email.'
    );
  });

  it('disables every activation button while a switch is in flight', () => {
    hooks.activation.mockReturnValue(mutation({ isPending: true, variables: 'personal' }));
    render(<CodexAuthProfilesCard />);
    expect(screen.getByText('Activating...')).toBeInTheDocument();
    for (const name of ['personal', 'work', 'unsigned']) {
      expect(screen.getByRole('button', { name: `Activate ${name}` })).toBeDisabled();
    }
  });

  it('displays safe failure text and a successful activation announcement', () => {
    hooks.activation.mockReturnValue(
      mutation({ error: new Error('Codex is busy. Try again when work finishes.') })
    );
    const { rerender } = render(<CodexAuthProfilesCard />);
    expect(screen.getByRole('alert')).toHaveTextContent('Codex is busy.');
    hooks.activation.mockReturnValue(
      mutation({ isSuccess: true, data: { email: 'personal@example.test' } })
    );
    rerender(<CodexAuthProfilesCard />);
    expect(screen.getByRole('status')).toHaveTextContent(
      'Activated personal@example.test for the VM.'
    );
  });

  it('displays live identity even with an empty registry', () => {
    hooks.profiles.mockReturnValue({
      data: { ...summary, profiles: [], activated: { ...summary.activated, name: null } },
      isLoading: false,
      error: null,
    });
    render(<CodexAuthProfilesCard />);
    expect(screen.getByText('work@example.test')).toBeInTheDocument();
    expect(screen.getByText('(unknown)')).toBeInTheDocument();
  });

  it('associates subscription usage by saved profile name rather than API response order', () => {
    hooks.quotas.mockReturnValue({
      data: {
        profiles: [
          {
            profileName: 'work',
            status: 'available',
            windows: [{ key: 'primary', label: '5-hour limit', usedPercent: 82 }],
          },
          {
            profileName: 'personal',
            status: 'available',
            windows: [{ key: 'secondary', label: 'Weekly limit', usedPercent: 14 }],
          },
        ],
      },
      isLoading: false,
      error: null,
    });
    render(<CodexAuthProfilesCard />);
    const personal = screen.getByLabelText('personal subscription usage');
    const work = screen.getByLabelText('work subscription usage');
    expect(within(personal).getByRole('progressbar', { name: 'Weekly limit' })).toHaveAttribute(
      'aria-valuenow',
      '14'
    );
    expect(within(work).getByRole('progressbar', { name: '5-hour limit' })).toHaveAttribute(
      'aria-valuenow',
      '82'
    );
    const missing = screen.getByLabelText('unsigned subscription usage');
    expect(within(missing).queryByRole('progressbar')).not.toBeInTheDocument();
    expect(missing).not.toHaveTextContent('0%');
  });

  it('shows quota reauthentication without disabling a valid saved-account switch', async () => {
    const activate = mutation();
    hooks.activation.mockReturnValue(activate);
    hooks.quotas.mockReturnValue({
      data: {
        profiles: [
          {
            profileName: 'personal',
            status: 'reauth_required',
            windows: [],
            message: 'Sign in again to read subscription usage.',
          },
        ],
      },
      isLoading: false,
      error: null,
    });
    render(<CodexAuthProfilesCard />);
    expect(screen.getByLabelText('personal subscription usage')).toHaveTextContent(
      'Sign in again to read subscription usage.'
    );
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Activate personal' }));
    expect(activate.mutate).toHaveBeenCalledWith('personal');
  });

  it('keeps account identities and activation available when the quota request fails', () => {
    hooks.quotas.mockReturnValue({
      data: undefined,
      isLoading: false,
      error: new Error('Subscription usage request failed'),
    });
    render(<CodexAuthProfilesCard />);
    expect(screen.getByRole('button', { name: 'Activate personal' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Activate work' })).toBeDisabled();
    expect(screen.getByText('personal@example.test')).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
  });

  it('retains a cached quota measurement when its refresh fails', () => {
    hooks.quotas.mockReturnValue({
      data: {
        profiles: [
          {
            profileName: 'personal',
            status: 'available',
            windows: [{ key: 'primary', label: '5-hour limit', usedPercent: 31 }],
          },
        ],
      },
      isLoading: false,
      error: new Error('Subscription usage refresh failed'),
    });
    render(<CodexAuthProfilesCard />);
    const personal = screen.getByLabelText('personal subscription usage');
    expect(within(personal).getByRole('progressbar')).toHaveAttribute('aria-valuenow', '31');
    expect(screen.getByRole('button', { name: 'Activate personal' })).toBeEnabled();
  });
});
