import { createHash } from 'crypto';
import type { Response } from 'express';

/** Clock-only fields tick in the client from their timestamps; all data and state are hashed. */
export function sendRevalidatedView(res: Response, view: unknown): void {
  const identity = JSON.stringify(view, (key, value: unknown) =>
    key === 'generatedAt' || key === 'ageSeconds' || key === 'cacheAgeSeconds' ? undefined : value
  );
  const etag = `W/"${createHash('sha256').update(identity).digest('hex')}"`;
  res.setHeader('Cache-Control', 'private, no-cache');
  res.setHeader('ETag', etag);
  res.vary('Cookie');
  // A no-cache request asks us to validate. Express's req.fresh instead forces
  // a 200 for that header, so compare the supplied validators explicitly.
  const validators = res.req.get('If-None-Match');
  if (
    validators?.split(',').some((tag) => {
      const value = tag.trim();
      return value === '*' || value.replace(/^W\//, '') === etag.replace(/^W\//, '');
    })
  ) {
    res.status(304).end();
    return;
  }
  res.json(view);
}
