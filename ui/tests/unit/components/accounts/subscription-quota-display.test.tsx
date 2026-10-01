import { describe, expect, it } from 'vitest';
import { render, screen } from '@tests/setup/test-utils';
import { SubscriptionQuotaDisplay } from '@/components/accounts/subscription-quota-display';

describe('SubscriptionQuotaDisplay', () => {
  it('labels reported usage and reset times with a timestamped cache indicator', () => {
    render(
      <SubscriptionQuotaDisplay
        cached
        quota={{
          status: 'available',
          windows: [
            {
              key: 'five_hour',
              label: 'Primary',
              usedPercent: 35.5,
              resetsAt: '2026-10-01T05:00:00Z',
            },
          ],
          fetchedAt: '2026-10-01T00:00:00Z',
          sampledAt: '2026-09-30T23:00:00Z',
        }}
      />
    );
    expect(screen.getByText('35.5% used')).toBeInTheDocument();
    expect(screen.getByRole('progressbar', { name: '5-hour limit' })).toHaveAttribute(
      'aria-valuenow',
      '35.5'
    );
    expect(screen.getByText('Cached')).toBeInTheDocument();
    expect(screen.getByText(/Resets /)).toBeInTheDocument();
    expect(screen.getByText(/Fetched /)).toBeInTheDocument();
    expect(screen.getByText(/Recorded /)).toBeInTheDocument();
  });

  it('does not synthesize zero usage for absent or invalid percentages', () => {
    const { rerender } = render(<SubscriptionQuotaDisplay />);
    expect(screen.getByText('Subscription usage unavailable.')).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    rerender(
      <SubscriptionQuotaDisplay
        quota={{
          status: 'available',
          windows: [{ key: 'five_hour', label: 'Primary', usedPercent: Number.NaN }],
        }}
      />
    );
    expect(screen.getByText('Subscription usage unavailable.')).toBeInTheDocument();
    expect(screen.queryByText('0% used')).not.toBeInTheDocument();
  });

  it('shows reauthentication guidance independently of the account controls', () => {
    render(<SubscriptionQuotaDisplay quota={{ status: 'reauth_required', windows: [] }} />);
    expect(screen.getByText('Sign in again to retrieve subscription usage.')).toBeInTheDocument();
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
  });

  it('preserves usage above a limit and clamps only the visual bar', () => {
    render(
      <SubscriptionQuotaDisplay
        quota={{
          status: 'available',
          windows: [{ key: 'extra', label: 'Extra usage', usedPercent: 125 }],
        }}
      />
    );
    expect(screen.getByText('125% used')).toBeInTheDocument();
    expect(screen.getByRole('progressbar', { name: 'Extra usage' })).toHaveAttribute(
      'aria-valuenow',
      '100'
    );
    expect(screen.getByRole('progressbar', { name: 'Extra usage' })).toHaveAttribute(
      'aria-valuetext',
      '125% used'
    );
  });
});
