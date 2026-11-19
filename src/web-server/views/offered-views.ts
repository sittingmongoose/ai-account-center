/** D7: the views this server builds, named on the dashboard answer the page reads first (`server.views`). */
import type { Response } from 'express';
import { RustService } from '../rust-service';
import { rustGroupOn } from '../rust-service-groups';
import type { AccountDashboard } from '../services/account-dashboard-types';

/** R1: each service's `groups.home`, read once (config.yaml is not read per request). */
const homeGroups = new WeakMap<RustService, boolean>();

function homeGroup(service: RustService): boolean {
  let on = homeGroups.get(service);
  if (on === undefined) {
    on = rustGroupOn('home');
    homeGroups.set(service, on);
  }
  return on;
}

/**
 * The service that builds Home (GET /api/views/home): the Rust service on, with the Home group on (R1,
 * `rustService.groups.home`, default-ready: on when unset) or `AAC_RUST_HOME_VIEW=on` (D7). Off, the route is
 * not offered.
 */
export function homeViewService(res: Response): RustService | null {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService &&
    service.mode === 'on' &&
    (process.env.AAC_RUST_HOME_VIEW === 'on' || homeGroup(service))
    ? service
    : null;
}

/**
 * R1: the service that answers Home from fed generations: the Home group on, under mode on. Otherwise Home is
 * D7's path (three Rust reads and the envelope), or not offered.
 */
export function homeFedService(res: Response): RustService | null {
  const service = homeViewService(res);
  return service && homeGroup(service) ? service : null;
}

/** The dashboard answer with `server.views` when this server builds Home; unchanged otherwise. */
export function withOfferedViews(dashboard: AccountDashboard, res: Response): AccountDashboard {
  if (!homeViewService(res)) return dashboard;
  return {
    ...dashboard,
    server: { ...(dashboard.server ?? { version: '', commit: null }), views: ['home'] },
  };
}
