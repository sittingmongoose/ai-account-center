// Whole-page images of Accounts & Settings in the app frame, in each format and theme, on the kit's
// real test server (../browser-check.config.mjs) with the fake account server of ../accounts-fixture.mjs.
// Needs dist/ built, like the kit.
//   browser-check run web-dashboard/tests/browser/accounts-images --set out=<folder>
import path from 'node:path';
import kit from '../browser-check.config.mjs';

// The kit's ctx as seen from its own folder. A view, not a copy: the runner fills in ctx.dir after this runs.
export default (ctx) => {
	const kitDir = path.resolve(ctx.configDir, '..');
	return kit(new Proxy(ctx, { get: (target, key) => (key === 'configDir' ? kitDir : target[key]) }));
};
