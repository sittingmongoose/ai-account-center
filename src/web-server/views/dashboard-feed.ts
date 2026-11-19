/** G2: Accounts and trays render the same Rust-held generation as Home. */
import type { Request, Response } from 'express';
import type { RustService } from '../rust-service';
import type { AccountDashboard } from '../services/account-dashboard-types';
import { ensureDashboardFeed, feedsOf, passFaithful } from './home-feed';

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
  if (typeof res.locals.onRustDashboard === 'function') {
    try {
      res.locals.onRustDashboard(platform, JSON.parse(reply.bytes.toString('utf8')));
    } catch {
      // The cache consumer must never change the Accounts reply.
    }
  }
  // Preserve Express's existing content type, weak ETag and conditional GET for the trays.
  res.type('application/json; charset=utf-8').send(reply.bytes);
  return true;
}
