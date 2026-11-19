import type { Request, Response } from 'express';
import { createApiRouter } from './api-router';
import { authKind } from '../middleware/request-auth';
import { ConfigError, ProfileError } from '../../errors/error-types';
import {
  isDashboardWebSocketOriginAllowed,
  requireDashboardSession,
} from '../middleware/auth-middleware';
import {
  CLAUDE_PROFILE_ID_PATTERN,
  listClaudeDesktopProfileMetadata,
} from '../services/claude-desktop-profile-service';
import {
  assertClaudeDesktopOpenAllowed,
  CLAUDE_HISTORY_UNCONFIRMED_MESSAGE,
  ClaudeHistoryOpenHeldError,
  claudeOpenUsesManagedHistory,
  openClaudeDesktopProfile,
} from '../services/claude-desktop-open-service';
import {
  getClaudeOpenOperations,
  type ClaudeOpenOperation,
} from '../services/claude-open-operations';
import { ClaudeDesktopTransportError } from '../services/claude-desktop-transport';
import { cleanupAllHistorySnapshots } from '../services/claude-history-sync-service';
import {
  composeClaudeDesktopUsage,
  getClaudeDesktopUsage,
  type ClaudeDesktopUsage,
} from '../services/claude-desktop-usage-service';
import { getCcsDir } from '../../utils/config-manager';
import {
  composeWithRust,
  readOwnedValueWithRust,
  readOwnedWithRust,
  rustWritesEnabled,
  RustService,
} from '../rust-service';
import {
  acceptRustDesktopProfiles,
  acceptRustDesktopProfilesWithOperations,
  isRustDesktopUsage,
  rustDesktopUsageInput,
} from '../rust-profile-input';

const router = createApiRouter();

/**
 * Whether the client asked for the asynchronous Open (`Prefer: respond-async`,
 * RFC 7240). It is a header, so the strict body-key check stays unchanged, and
 * clients that do not send it (the shipped web UI and both trays) keep the
 * synchronous 200 or refusal they already handle.
 */
export function prefersRespondAsync(header: string | string[] | undefined): boolean {
  const values = Array.isArray(header) ? header : header === undefined ? [] : [header];
  return values.some(
    (value) =>
      value.length <= 1024 &&
      value
        .split(',')
        .some((preference) => preference.split(';')[0].trim().toLowerCase() === 'respond-async')
  );
}

/** G13: whether this request's service bound the Rust Open owner (mode on + group on). */
function claudeOpenDelegates(res: Response): boolean {
  const service: unknown = res.locals.rustService;
  return service instanceof RustService && service.claudeOpenEnabled;
}

/**
 * GET /desktop-profiles' read, after its caller's session guard: the M3 Rust service reads the
 * manifest through its capped descriptor read (part 4) and TS adds each Open's progress; the TS
 * reader is the fallback. GET /api/views/home (D7) reads Home's profiles through it too.
 */
function desktopProfilesRead(rustOpenEnabled: boolean) {
  const operations = getClaudeOpenOperations();
  const scope = getCcsDir();
  // `openOperation` is the progress of a 202 Open (null when none is known). Only a
  // profile with a manifest id can be opened, so only those rows carry the field. It is
  // this process's memory, so TS adds it to the manifest rows whoever read them.
  const withOperations = <T extends { id?: unknown }>(profiles: T[]) =>
    profiles.map((profile) =>
      typeof profile.id === 'string' && profile.id
        ? { ...profile, openOperation: operations.forProfile(scope, profile.id) }
        : profile
    );
  return {
    route: '/api/claude/desktop-profiles',
    scope,
    accept: rustOpenEnabled
      ? acceptRustDesktopProfilesWithOperations
      : acceptRustDesktopProfiles(withOperations),
    read: async () => ({ profiles: withOperations(await listClaudeDesktopProfileMetadata()) }),
  };
}

/** Home's profiles (D7): GET /desktop-profiles' list, read the same way; throws when it cannot be. */
export async function readClaudeDesktopProfiles(req: Request, res: Response): Promise<unknown[]> {
  const { route, scope, accept, read } = desktopProfilesRead(claudeOpenDelegates(res));
  const body = await readOwnedValueWithRust(req, res, route, scope, accept, read);
  const profiles = (body as { profiles?: unknown } | undefined)?.profiles;
  if (!Array.isArray(profiles)) throw new ProfileError('Claude desktop profiles were not read.');
  return profiles;
}

router.get('/desktop-profiles', async (req, res): Promise<void> => {
  if (!requireDashboardSession(req, res)) return;
  const { route, scope, accept, read } = desktopProfilesRead(claudeOpenDelegates(res));
  // After the guard above, the M3 Rust service may read the same manifest; this is the fallback.
  const legacy = async (): Promise<unknown> => {
    try {
      const body = await read();
      res.json(body);
      return body;
    } catch {
      res.status(500).json({ error: 'Claude desktop profiles could not be read safely.' });
      return undefined;
    }
  };
  await readOwnedWithRust(req, res, route, scope, accept, legacy);
});

router.get('/desktop-profiles/usage', async (req, res): Promise<void> => {
  if (!requireDashboardSession(req, res)) return;
  const platform = req.query.platform;
  if (platform !== 'mac' && platform !== 'windows') {
    res.status(400).json({ error: 'Select a valid Claude desktop platform.' });
    return;
  }
  try {
    // The manifest and remote history reads stay in TS; the M3 Rust service may compose them.
    res.json(
      await getClaudeDesktopUsage(
        platform,
        async (facts) =>
          (await composeWithRust(
            req,
            res,
            '/api/claude/desktop-profiles/usage',
            () => rustDesktopUsageInput(facts),
            () => composeClaudeDesktopUsage(facts),
            isRustDesktopUsage
          )) as ClaudeDesktopUsage
      )
    );
  } catch {
    res.status(500).json({ error: 'Claude desktop usage could not be read safely.' });
  }
});

router.post('/desktop-profiles/:id/open', async (req, res): Promise<void> => {
  if (authKind(req) === null) {
    res.status(401).json({ error: 'Authentication required' });
    return;
  }
  if (!isDashboardWebSocketOriginAllowed(req)) {
    res.status(403).json({ error: 'Claude account launch requires the dashboard origin.' });
    return;
  }
  if (!req.is('application/json')) {
    res.status(415).json({ error: 'Claude account launch requires application/json.' });
    return;
  }
  // Membership (pending registry or manifest) and per-platform launchers are
  // resolved by the Open operation below (404 unknown, 409 unconfigured);
  // this only rejects unsafe IDs and malformed bodies before any lookup.
  if (
    !CLAUDE_PROFILE_ID_PATTERN.test(req.params.id) ||
    !req.body ||
    (req.body.platform !== 'mac' && req.body.platform !== 'windows') ||
    Object.keys(req.body).some((key) => key !== 'platform')
  ) {
    res.status(400).json({ error: 'Select a configured Claude desktop account and platform.' });
    return;
  }

  const id = req.params.id;
  const platform: 'mac' | 'windows' = req.body.platform;
  const respondAsync = prefersRespondAsync(req.headers.prefer);
  // G13: the bound Rust Open owner runs the whole flow (progress, hold, copy, launcher);
  // the unbound handoff keeps the TypeScript flow below. A lost reply may follow a started
  // Open, so it answers 500 here and never replays the click.
  if (claudeOpenDelegates(res)) {
    const service = res.locals.rustService as RustService;
    try {
      const reply = await service.claudeOpen(id, platform, respondAsync);
      if (reply !== null) {
        if (reply.status === 202) res.setHeader('Preference-Applied', 'respond-async');
        res.status(reply.status).json(reply.body);
        return;
      }
    } catch {
      res.status(500).json({ error: 'Claude account could not be opened safely.' });
      return;
    }
  }
  const scope = getCcsDir();
  const operations = getClaudeOpenOperations();
  const accepted = (operation: { state: string; id: string }): void => {
    res.setHeader('Preference-Applied', 'respond-async');
    res.status(202).json({ id, platform, state: operation.state, operationId: operation.id });
  };
  // Wait for this Open's end and answer as before: 200, or the same refusal.
  const awaitOutcome = async (): Promise<void> => {
    const outcome = await operations.settled(scope, id, platform);
    if (outcome && !outcome.ok) throw outcome.error;
    res.json({ opened: true, id, platform });
  };
  // The Open running for this exact scope, profile and platform, if any.
  const runningOpen = (): ClaudeOpenOperation | null => operations.running(scope, id, platform);
  /**
   * The Open this click answers with: a running one it joins, or the managed one it
   * starts; null once the ordinary Open (no managed history copy) is done. Another
   * click can start the managed Open while this one awaits, and that Open's history
   * copy then holds Open with its marker. So the running Open is looked up again after
   * every await, and a held refusal joins it when there is one. A hold with no Open
   * running for this exact scope, profile and platform is durable and keeps its 409.
   */
  const startOrJoin = async (): Promise<ClaudeOpenOperation | null> => {
    try {
      const managed = await claudeOpenUsesManagedHistory(id, platform);
      const joined = runningOpen();
      if (joined) return joined;
      if (!managed) {
        // No managed history copy: the ordinary Open, answered when it is done.
        await openClaudeDesktopProfile(id, platform);
        return null;
      }
      // The same refusals as before, answered before any work starts.
      await assertClaudeDesktopOpenAllowed(id, platform);
    } catch (error) {
      // Only the hold of an Open running now is joined; every other refusal stands.
      const joined = error instanceof ClaudeHistoryOpenHeldError ? runningOpen() : null;
      if (joined) return joined;
      throw error;
    }
    // Every managed Open is tracked, so a polling client sees its progress too.
    return (
      runningOpen() ??
      operations.start(scope, id, platform, (observer) =>
        openClaudeDesktopProfile(id, platform, observer)
      )
    );
  };
  try {
    // A click while its Open still runs joins it; the copy's own hold must not refuse it.
    const operation = runningOpen() ?? (await startOrJoin());
    if (!operation) res.json({ opened: true, id, platform });
    else if (respondAsync) accepted(operation);
    else await awaitOutcome();
  } catch (error) {
    if (error instanceof ProfileError) {
      res.status(404).json({ error: 'Claude desktop profile was not found.' });
    } else if (error instanceof ClaudeHistoryOpenHeldError) {
      // Same 409 as before, now with the reason the clients should show.
      res
        .status(409)
        .json({ error: CLAUDE_HISTORY_UNCONFIRMED_MESSAGE, code: 'history_unconfirmed' });
    } else if (error instanceof ConfigError) {
      res
        .status(409)
        .json({ error: 'Claude desktop launcher is not configured for this platform.' });
    } else if (error instanceof ClaudeDesktopTransportError) {
      res.status(error.timedOut ? 504 : 502).json({ error: error.message });
    } else {
      res.status(500).json({ error: 'Claude account could not be opened safely.' });
    }
  }
});

router.post('/history-snapshots/cleanup', async (req, res): Promise<void> => {
  if (!requireDashboardSession(req, res)) return;
  if (!isDashboardWebSocketOriginAllowed(req)) {
    res.status(403).json({ error: 'History snapshot cleanup requires the dashboard origin.' });
    return;
  }
  if (!req.is('application/json')) {
    res.status(415).json({ error: 'History snapshot cleanup requires application/json.' });
    return;
  }
  if (
    !req.body ||
    typeof req.body !== 'object' ||
    Array.isArray(req.body) ||
    Object.keys(req.body).length !== 0
  ) {
    res.status(400).json({ error: 'History snapshot cleanup takes no fields.' });
    return;
  }
  try {
    if (claudeOpenDelegates(res)) {
      const service = res.locals.rustService as RustService;
      const reply = await service.claudeHistoryCleanup();
      if (reply !== null) {
        res.status(reply.status).json(reply.body);
        return;
      }
    }
    res.json(await cleanupAllHistorySnapshots(undefined, rustWritesEnabled(res)));
  } catch {
    res.status(500).json({ error: 'History snapshots could not be cleaned safely.' });
  }
});

export default router;
