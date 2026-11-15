/**
 * GET /api/views/home (D7, `AAC_RUST_HOME_VIEW=on` with `AAC_RUST_SERVICE=on`; otherwise not offered and a 404 as
 * before). One read for Home: the three answers the page reads today (GET /api/accounts/dashboard,
 * /api/claude/desktop-profiles and /api/antigravity/profiles), each read the way its own route answers it (the
 * profiles through the Rust service's capped manifest read, the Antigravity copy through its composition, both with
 * the TS fallback), and, when Rust can prove it equal to the page's own, the finished view model the Rust service
 * built from them. The page keeps the data (Details, Accounts and the notices read it) and draws Home from `view`, or
 * builds its own view when `view` is null: the profiles could not be read, the page's context (`X-AAC-Home-Context`)
 * could not be, or Rust refused it (the shared read fallback).
 *
 * R1 (`rustService.groups.home: on`): the same answer from fed generations (views/home-feed.ts). One dashboard
 * read serves both paths: the fed request carries only its generation ids and clock, Rust answers the final bytes
 * and ETag, and on any decline the handler below answers from that same read, as before.
 */
import type { NextFunction, Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { requireDashboardSession } from '../middleware/auth-middleware';
import { isSecureTransport } from '../middleware/secure-transport';
import {
  getAccountDashboard,
  readAccountDashboardForHome,
  withPassedResetsAt,
  type HomeDashboardRead,
} from '../services/account-dashboard-service';
import { getCcsDir } from '../../utils/config-manager';
import { readClaudeDesktopProfiles } from './claude-desktop-routes';
import { readAntigravityInventory, runtimeInventory } from './antigravity-routes';
import { homeFedService, homeViewService, withOfferedViews } from '../views/offered-views';
import { homeReadInput, isHomeView, parseHomeContext } from '../views/home-context';
import { answerHomeFed } from '../views/home-feed';

export interface HomeViewRouterDeps {
  getDashboard?: typeof getAccountDashboard;
  readProfiles?: (req: Request, res: Response) => Promise<unknown[]>;
  readInventory?: (req: Request, res: Response, read?: { value: unknown }) => Promise<unknown>;
  /** R1: the dashboard before its per-response pass, with that pass's clock. */
  readHomeDashboard?: typeof readAccountDashboardForHome;
  /** R1: the Antigravity runtime's inventory. */
  runtimeInventory?: () => Promise<unknown>;
  /** R1: the CCS directory the TS readers use. */
  scope?: () => string;
}

export function createHomeViewRouter(deps: HomeViewRouterDeps = {}): Router {
  const router = createApiRouter();
  router.get('/home', async (req: Request, res: Response, next: NextFunction): Promise<void> => {
    const service = homeViewService(res);
    if (!service) {
      next();
      return;
    }
    if (authKind(req) === null || !requireDashboardSession(req, res)) {
      if (!res.headersSent) res.status(401).json({ error: 'Authentication required' });
      return;
    }
    const platform = req.query.platform ?? 'mac';
    const refresh = req.query.refresh ?? 'false';
    if (
      (platform !== 'mac' && platform !== 'windows') ||
      (refresh !== 'true' && refresh !== 'false') ||
      Object.keys(req.query).some((key) => key !== 'platform' && key !== 'refresh')
    ) {
      res.status(400).json({ error: 'Select a valid desktop platform and refresh value.' });
      return;
    }
    // A context this server cannot read (an unusual zone, a long name) only means the page builds its own view.
    const context = parseHomeContext(req.get('x-aac-home-context'), platform);
    res.setHeader('Cache-Control', 'no-store');
    const secureTransport = isSecureTransport(req);
    // R1: one read for both paths; `undefined` when the fed path did not run.
    let read: HomeDashboardRead | null | undefined;
    // R1: the runtime inventory the fed path read, so a fallback answers from the same reads.
    let fedInventory: { value: unknown } | undefined;
    const fed = homeFedService(res);
    if (fed) {
      const [homeRead, inventory] = await Promise.all([
        (deps.readHomeDashboard ?? readAccountDashboardForHome)(platform, refresh === 'true', {
          secureTransport,
        }).catch(() => null),
        (deps.runtimeInventory ?? runtimeInventory)().then(
          (value) => ({ value }),
          () => null
        ),
      ]);
      read = homeRead;
      fedInventory = inventory ?? undefined;
      if (homeRead && inventory) {
        const answered = await answerHomeFed(req, res, fed, {
          platform,
          secure: secureTransport,
          context,
          read: { dashboard: withOfferedViews(homeRead.dashboard, res), now: homeRead.now },
          inventory: inventory.value,
          scope: (deps.scope ?? getCcsDir)(),
        });
        if (answered) return;
      } else fed.fedDeclined('/api/views/home', 'input');
    }
    const [dashboard, profiles, antigravity] = await Promise.all([
      // The same read's answer after its pass; a pass that throws is a failed read, as `get` rejects.
      read !== undefined
        ? Promise.resolve(read)
            .then((value) => value && withPassedResetsAt(value.dashboard, value.now))
            .catch(() => null)
        : (deps.getDashboard ?? getAccountDashboard)(platform, refresh === 'true', {
            secureTransport,
          }).catch(() => null),
      (deps.readProfiles ?? readClaudeDesktopProfiles)(req, res).catch(() => null),
      (deps.readInventory ?? readAntigravityInventory)(req, res, fedInventory).catch(() => null),
    ]);
    if (!dashboard) {
      res.status(500).json({ error: 'Account usage could not be read safely.' });
      return;
    }
    const offered = withOfferedViews(dashboard, res);
    // Without the profiles the page keeps the ones it has, which Rust does not know: the page draws its own view.
    // The shared M3 bridge: Rust composes from the presentation envelope; a refusal, a fault or any answer that is
    // not a view is the logged fallback, whose "legacy" here is no view (the page builds its own).
    const view: unknown =
      context && profiles
        ? await service.compose(
            req,
            '/api/views/home',
            () => homeReadInput(context, offered, profiles, antigravity),
            () => null,
            isHomeView
          )
        : null;
    res.json({ schemaVersion: 1, dashboard: offered, profiles, antigravity, view });
  });
  return router;
}

export default createHomeViewRouter();
