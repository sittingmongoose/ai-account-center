// The app frame's sign-in on its own test server: the kit's config (../browser-check.config.mjs, a temporary home with
// sign-in not set up yet), in a folder of its own because the limiter step closes the server's sign-in for 15 minutes.
//   browser-check run web-dashboard/tests/browser/frame-signin
import path from 'node:path';
import kit from '../browser-check.config.mjs';

export default (ctx) => {
	// The kit's ctx as seen from its own folder. A view, not a copy: the runner fills in ctx.dir after this runs.
	const kitDir = path.resolve(ctx.configDir, '..');
	const config = kit(new Proxy(ctx, { get: (target, key) => (key === 'configDir' ? kitDir : target[key]) }));
	// Refused on purpose here: a wrong password (401), the limiter (429), and the readings of a session that ended (401).
	config.consoleErrors = { ...config.consoleErrors, allow: [...config.consoleErrors.allow, /status of 401 .*\/api\//, /status of 429 .*\/api\/auth\/login/] };
	// Sign-in turned on with no password yet, as a fresh install with sign-in on: the first visit gets the setup form.
	const server = config.server;
	config.server = (c, s) => {
		const v = server(c, s);
		return { ...v, env: { ...v.env, CCS_DASHBOARD_AUTH_ENABLED: '1' } };
	};
	return config;
};
