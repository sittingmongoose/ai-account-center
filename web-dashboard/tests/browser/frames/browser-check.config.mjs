// Whole-page images of the Leptos Status and Scoreboard pages in the five approved formats, from the design fixtures
// (web-dashboard/leptos/fixtures, made-up data) on the fixture server, for layout review against the mockups.
//   browser-check run web-dashboard/tests/browser/frames --set out=<folder>
// Needs web-dashboard/leptos/pkg built (see web-dashboard/leptos/NOTES.md). Output: <page>-<format>-<theme>.png.
import path from 'node:path';

export default async (ctx) => {
	const leptos = path.resolve(ctx.configDir, '..', '..', '..', 'leptos');
	const port = await ctx.freePort();
	return {
		name: 'aac-leptos-frames',
		server: {
			command: [process.execPath, path.join(leptos, 'dev', 'serve.mjs'), String(port)],
			url: `http://${ctx.reachHost}:${port}/app/status`,
			timeoutMs: 30000,
		},
		viewport: { width: 1440, height: 900 },
		isolate: true,
		// The fixture server has no /ws: the page's hint socket retrying is expected.
		consoleErrors: { fail: false },
		failedRequests: { fail: false },
	};
};
