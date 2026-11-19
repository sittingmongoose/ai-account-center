import { timingSafeEqual } from 'node:crypto';
import type { IncomingMessage } from 'node:http';
import type { Request, RequestHandler } from 'express';
import type { DeviceRequestAuth } from './middleware/request-auth';

type Principal =
  | { kind: 'anonymous' }
  | { kind: 'session'; username: string; id: string }
  | { kind: 'device'; id: string; platform: 'mac' | 'windows' };

interface CompositionAccess {
  principal: Principal;
  authEnabled: boolean;
  secure: boolean;
  sameOrigin: boolean;
  transport: 'encrypted' | 'loopback' | 'plain';
  local: boolean;
  directLoopback: boolean;
  lanProxy: boolean;
  client: string | null;
}

const access = new WeakMap<IncomingMessage, CompositionAccess>();

export function compositionAccess(req: IncomingMessage): CompositionAccess | undefined {
  return access.get(req);
}

/** First middleware on the private loopback listener. Trust starts with its per-boot secret. */
export function compositionGuard(proof: string): RequestHandler {
  const expected = Buffer.from(proof, 'hex');
  return (req, res, next) => {
    const presented = req.headers['x-aac-compose-proof'];
    const encoded = req.headers['x-aac-compose-access'];
    delete req.headers['x-aac-compose-proof'];
    delete req.headers['x-aac-compose-access'];
    if (
      req.socket.remoteAddress !== '127.0.0.1' ||
      !/^\/api(?:\/|$)/i.test(req.path) ||
      typeof presented !== 'string' ||
      !/^[0-9a-f]{64}$/.test(presented) ||
      !timingSafeEqual(Buffer.from(presented, 'hex'), expected) ||
      typeof encoded !== 'string' ||
      encoded.length > 4096 ||
      !/^[A-Za-z0-9_-]+$/.test(encoded)
    ) {
      res.sendStatus(404);
      return;
    }
    let context: CompositionAccess;
    try {
      context = JSON.parse(Buffer.from(encoded, 'base64url').toString('utf8')) as CompositionAccess;
    } catch {
      res.sendStatus(404);
      return;
    }
    if (!valid(context)) {
      res.sendStatus(404);
      return;
    }
    access.set(req, context);
    const principal = context.principal;
    if (principal.kind === 'session') {
      // Auth routes and the session store remain on the Rust listener.
      req.session = { authenticated: true, username: principal.username } as Request['session'];
      req.sessionID = principal.id;
    } else if (principal.kind === 'device') {
      (req as Request & { auth: DeviceRequestAuth }).auth = {
        kind: 'device',
        deviceId: principal.id,
        platform: principal.platform,
        viaPreviousToken: false,
        tokenSha256: '',
      };
    }
    next();
  };
}

function valid(context: CompositionAccess | null): context is CompositionAccess {
  if (!context || typeof context !== 'object') return false;
  const p = context.principal;
  return (
    !!p &&
    (p.kind === 'anonymous' ||
      (p.kind === 'session' &&
        typeof p.username === 'string' &&
        typeof p.id === 'string' &&
        p.id.length > 0) ||
      (p.kind === 'device' &&
        typeof p.id === 'string' &&
        (p.platform === 'mac' || p.platform === 'windows'))) &&
    typeof context.authEnabled === 'boolean' &&
    typeof context.secure === 'boolean' &&
    typeof context.sameOrigin === 'boolean' &&
    ['encrypted', 'loopback', 'plain'].includes(context.transport) &&
    typeof context.local === 'boolean' &&
    typeof context.directLoopback === 'boolean' &&
    typeof context.lanProxy === 'boolean' &&
    (context.client === null || typeof context.client === 'string')
  );
}
