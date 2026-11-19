/** G2: Accounts and trays render the same Rust-held generation as Home. */
import type { Request, Response } from 'express';
import { publishCollectionFeed } from '../rust-service-transport';
import type { RustService } from '../rust-service';
import type { AccountDashboard } from '../services/account-dashboard-types';
import {
  dashboardPublication,
  ensureDashboardFeed,
  feedsOf,
  nextGeneration,
  passFaithful,
} from './home-feed';

const ROUTE = '/api/accounts/dashboard';

export async function answerDashboardFed(
  req: Request,
  res: Response,
  service: RustService,
  input: {
    platform: 'mac' | 'windows';
    secure: boolean;
    dashboard: AccountDashboard;
    now: number;
    scope: string;
  }
): Promise<boolean> {
  const { dashboard, platform, secure, now, scope } = input;
  const boot = service.feedBoot(scope);
  if (
    !boot ||
    !Array.isArray(dashboard.accounts) ||
    typeof dashboard.updatedAt !== 'string' ||
    !passFaithful(dashboard)
  ) {
    service.fedDeclined(ROUTE, boot ? 'input' : 'root');
    return false;
  }
  const feeds = feedsOf(service, boot);
  const generation = await ensureDashboardFeed(service, feeds, platform, secure, dashboard);
  if ('declined' in generation) {
    service.fedDeclined(ROUTE, generation.declined);
    return false;
  }
  const reply = await service.fedView(
    req,
    ROUTE,
    {
      'x-aac-feed': JSON.stringify({
        platform,
        secure,
        now,
        updatedAt: dashboard.updatedAt,
        dashboard: generation.generation,
      }),
    },
    'accounts-dashboard@1'
  );
  if (reply === 'stale') feeds.held.clear();
  if (!reply || typeof reply === 'string') return false;
  // Preserve Express's existing content type, weak ETag and conditional GET for the trays.
  res.type('application/json; charset=utf-8').send(reply.bytes);
  return true;
}

/** The sampler uses this cache-only handoff after the regular dashboard pass. */
export async function publishSampledDashboard(
  scope: string,
  platform: 'mac' | 'windows',
  dashboard: AccountDashboard
): Promise<void> {
  const publication = dashboardPublication(dashboard);
  if (publication && passFaithful(dashboard)) {
    await publishCollectionFeed(
      scope,
      `home-dashboard-${platform}-0`,
      nextGeneration(),
      publication.data,
      publication.facts
    );
  }
}
