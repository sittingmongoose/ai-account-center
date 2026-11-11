// Synthetic session fixture; uses the actual API guard and deliverable handler, no collectors.
import express from 'express';
import fs from 'fs';
import path from 'path';
import { apiAuthMiddleware } from '../../../../src/web-server/middleware/auth-middleware';
import { createShareRouter } from '../../../../src/web-server/routes/share-route';
const [port, root] = process.argv.slice(2);
fs.mkdirSync(path.join(root, 'share'), { recursive: true });
fs.writeFileSync(path.join(root, 'share/concept.html'), `<!doctype html><h1>Concept</h1>
<script>document.body.dataset.executed='yes';try{document.body.dataset.stolen=document.cookie}catch{}
fetch('/api/private');</script><img src="/api/private"><iframe src="/api/private"></iframe>`);
let hits = 0;
const app = express();
app.get('/', (_req, res) => res.send('<h1>Sandbox fixture</h1>'));
app.get('/hits', (_req, res) => res.json({ hits }));
app.use((req, _res, next) => {
  if (req.headers.cookie?.includes('fixture=yes')) Object.assign(req, { session: { authenticated: true, epoch: 0 } });
  next();
});
app.use('/api', apiAuthMiddleware);
app.use('/api/status/deliverable', createShareRouter(() => root));
app.get('/api/private', (_req, res) => { hits++; res.json({ secret: 'fixture-only' }); });
app.listen(Number(port), '127.0.0.1');
