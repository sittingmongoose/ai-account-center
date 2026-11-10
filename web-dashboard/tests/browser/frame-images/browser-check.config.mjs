// Whole-page images of the app frame with each of its five tabs, in the five formats and both themes, on the kit's
// real test server (../browser-check.config.mjs: a temporary home, the Status and Scoreboard fixtures, synthetic
// accounts answered in the page). Needs dist/ built, like the kit.
//   browser-check run web-dashboard/tests/browser/frame-images --set out=<folder>
import path from 'node:path';
import kit from '../browser-check.config.mjs';

// The kit's ctx as seen from its own folder. A view, not a copy: the runner fills in ctx.dir after this runs.
export default (ctx) => {
	const kitDir = path.resolve(ctx.configDir, '..');
	return kit(new Proxy(ctx, { get: (target, key) => (key === 'configDir' ? kitDir : target[key]) }));
};
