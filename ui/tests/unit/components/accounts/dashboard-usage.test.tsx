import { describe, expect, it } from 'vitest';
import { render, screen } from '@tests/setup/test-utils';
import { DashboardUsage } from '@/components/accounts/dashboard-usage';
import type { DashboardAccount, DashboardUsageWindow } from '@/hooks/use-accounts-dashboard';

const window: DashboardUsageWindow = {
  key: 'weekly',
  label: 'Weekly',
  usedPercent: null,
  remainingPercent: null,
  resetAt: null,
  windowMinutes: null,
  used: null,
  limit: null,
  unit: null,
};
const account: DashboardAccount = {
  id: 'kimi:one',
  provider: 'kimi-code',
  providerLabel: 'Kimi Code',
  label: 'Kimi',
  email: 'test@example.test',
  plan: 'Pro',
  platform: 'mac',
  source: 'Kimi on Mac',
  status: 'ok',
  message: null,
  fetchedAt: null,
  sampledAt: null,
  isActive: false,
  windows: [],
  capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
};

describe('DashboardUsage', () => {
  it('does not turn unavailable usage into zero', () => {
    render(<DashboardUsage account={account} />);
    expect(screen.getByText('Subscription usage unavailable.')).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    expect(screen.queryByText('0% used')).not.toBeInTheDocument();
  });
  it.each([NaN, Infinity, -1, 101])(
    'does not render invalid percentage %s as a quota',
    (usedPercent) => {
      render(<DashboardUsage account={{ ...account, windows: [{ ...window, usedPercent }] }} />);
      expect(screen.getByText('Usage not reported')).toBeInTheDocument();
      expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    }
  );
  it('renders genuine zero usage and a supplied reset timestamp', () => {
    render(
      <DashboardUsage
        account={{
          ...account,
          windows: [{ ...window, usedPercent: 0, resetAt: '2026-10-01T12:00:00Z' }],
        }}
      />
    );
    expect(screen.getByText('0% used')).toBeInTheDocument();
    expect(screen.getByRole('progressbar', { name: 'Weekly' })).toHaveAttribute(
      'aria-valuenow',
      '0'
    );
    expect(screen.getByText(/^Resets /)).toHaveAttribute('datetime', '2026-10-01T12:00:00Z');
  });
  it('renders reported remaining percent without inventing reset times', () => {
    render(
      <DashboardUsage account={{ ...account, windows: [{ ...window, remainingPercent: 25 }] }} />
    );
    expect(screen.getByText('75% used')).toBeInTheDocument();
    expect(screen.getByText('Reset time unavailable')).toBeInTheDocument();
  });
  it('preserves reported token amounts when no percentage is supplied', () => {
    render(
      <DashboardUsage
        account={{ ...account, windows: [{ ...window, used: 1000, limit: 5000, unit: 'tokens' }] }}
      />
    );
    expect(screen.getByText('1K / 5K tokens')).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
  });
  it('labels retained samples after a refresh failure', () => {
    render(
      <DashboardUsage
        account={{ ...account, status: 'cached', windows: [{ ...window, usedPercent: 42 }] }}
        error
      />
    );
    expect(screen.getByText('42% used')).toBeInTheDocument();
    expect(
      screen.getByText('Refresh failed. Showing the last received sample.')
    ).toBeInTheDocument();
  });
});
