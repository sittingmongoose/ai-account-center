/** D7: the views this server builds, named on the dashboard answer the page reads first (`server.views`). */
import type { Response } from 'express';
import { RustService } from '../rust-service';
import type { AccountDashboard } from '../services/account-dashboard-types';

/**
 * The service that builds Home (GET /api/views/home): the Rust service on, with `AAC_RUST_HOME_VIEW=on`
 * (default off). Off, the route is not offered.
 */
export function homeViewService(res: Response): RustService | null {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService &&
    service.mode === 'on' &&
    process.env.AAC_RUST_HOME_VIEW === 'on'
    ? service
    : null;
}

/** The dashboard answer with `server.views` when this server builds Home; unchanged otherwise. */
export function withOfferedViews(dashboard: AccountDashboard, res: Response): AccountDashboard {
  if (!homeViewService(res)) return dashboard;
  return {
    ...dashboard,
    server: { ...(dashboard.server ?? { version: '', commit: null }), views: ['home'] },
  };
}
