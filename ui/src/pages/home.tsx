import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { ArrowUpRight, Loader2, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { CodexHomePanel } from '@/components/accounts/codex-home-panel';
import { ClaudeHomePanel } from '@/components/accounts/claude-home-panel';
import { UsageOnlyPanel } from '@/components/accounts/usage-only-panel';
import { readableUsageTime } from '@/lib/accounts-dashboard';
import { useAccountsDashboard, useRefreshAccountsDashboard } from '@/hooks/use-accounts-dashboard';
import type { DashboardPlatform } from '@/hooks/use-accounts-dashboard';

function initialPlatform(): DashboardPlatform {
  try {
    const saved = localStorage.getItem('ccs-claude-computer');
    if (saved === 'mac' || saved === 'windows') return saved;
  } catch {
    /* A browser may disable storage. */
  }
  return /Windows/i.test(navigator.userAgent) ? 'windows' : 'mac';
}

export function HomePage() {
  const [platform, setPlatform] = useState<DashboardPlatform>(initialPlatform);
  const dashboard = useAccountsDashboard(platform);
  const refresh = useRefreshAccountsDashboard(platform);
  const queryClient = useQueryClient();
  const accounts = dashboard.data?.accounts ?? [];
  const updated = readableUsageTime(dashboard.data?.updatedAt);
  const busy = dashboard.isFetching || refresh.isPending;
  const setComputer = (next: DashboardPlatform) => {
    setPlatform(next);
    try {
      localStorage.setItem('ccs-claude-computer', next);
    } catch {
      /* Optional preference. */
    }
  };
  const refreshAll = () => {
    refresh.mutate();
    for (const key of [
      'codex-auth-profiles',
      'codex-auth-profile-quotas',
      'codex-automatic-switch',
      'claude-desktop-profiles',
    ]) {
      void queryClient.invalidateQueries({ queryKey: [key] });
    }
  };
  return (
    <div className="mx-auto w-full max-w-7xl space-y-10 px-4 py-7 sm:px-6 sm:py-10 lg:px-8">
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div className="space-y-2">
          <p className="text-[11px] font-semibold uppercase tracking-[0.2em] text-accent">
            CCS account dashboard
          </p>
          <h1 className="text-3xl font-semibold tracking-tight sm:text-4xl">Your accounts</h1>
          <p className="max-w-2xl text-sm leading-6 text-muted-foreground">
            See subscription usage, open Claude profiles, and choose the Codex account running on
            Ubuntu.
          </p>
        </div>
        <div className="flex items-center gap-4">
          <div className="hidden text-right text-xs text-muted-foreground sm:block">
            <p>Refreshes every minute</p>
            {updated && <p className="mt-1">Updated {updated}</p>}
          </div>
          <Button variant="outline" onClick={refreshAll} disabled={busy} className="gap-2">
            {busy ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <RefreshCw className="h-4 w-4" />
            )}
            Refresh usage
          </Button>
        </div>
      </header>
      <nav className="grid grid-cols-3 gap-3 sm:hidden" aria-label="Jump to accounts">
        {[
          ['codex', 'Codex'],
          ['claude', 'Claude'],
          ['usage', 'Other accounts'],
        ].map(([id, label]) => (
          <a
            href={`#${id}`}
            key={id}
            className="flex items-center justify-between rounded-lg border bg-card px-3 py-2 text-xs font-medium"
          >
            {label}
            <ArrowUpRight className="h-3 w-3" />
          </a>
        ))}
      </nav>
      {(dashboard.error || refresh.error) && (
        <p
          role="alert"
          className="rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive"
        >
          Unable to refresh all account usage. Saved samples and account controls remain available.
        </p>
      )}
      <CodexHomePanel />
      <ClaudeHomePanel
        platform={platform}
        onPlatformChange={setComputer}
        accounts={accounts.filter((account) => account.provider === 'claude')}
        isUsageLoading={dashboard.isLoading}
        usageError={Boolean(dashboard.error || refresh.error)}
      />
      <UsageOnlyPanel
        accounts={accounts}
        loading={dashboard.isLoading}
        error={Boolean(dashboard.error || refresh.error)}
      />
      <footer className="border-t border-border/70 pt-5 text-xs leading-5 text-muted-foreground">
        Usage and reset times are reported by each provider. Cached or unavailable data is labelled
        on the account.
      </footer>
    </div>
  );
}
