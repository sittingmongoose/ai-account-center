import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@tests/setup/test-utils';
import { CodexAccountsPage } from '@/pages/codex-accounts';

const diagnostics = vi.hoisted(() =>
  vi.fn(() => {
    throw new Error('Codex diagnostics are unavailable');
  })
);

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
}));

describe('CodexAccountsPage', () => {
  it('shows all saved accounts and switch actions independently of Codex diagnostics', () => {
    render(<CodexAccountsPage />);

    expect(screen.getByRole('heading', { name: 'Codex accounts' })).toBeInTheDocument();
    expect(
      screen.getByText('Click Activate beside the account you want to use.')
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
});
