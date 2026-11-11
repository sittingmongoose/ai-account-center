export default async function (t) {
  await t.goto('/');
  await t.page.evaluate(() => { document.cookie = 'fixture=yes; SameSite=Strict; Path=/'; });
  await t.goto('/api/status/deliverable?path=share%2Fconcept.html');
  const result = await t.page.evaluate(async () => {
    let cookieDenied = false, fetchDenied = false;
    try { document.cookie; } catch { cookieDenied = true; }
    try { await fetch('/api/private', { credentials: 'include' }); } catch { fetchDenied = true; }
    return { cookieDenied, fetchDenied, executed: document.body.dataset.executed ?? null, heading: document.querySelector('h1')?.textContent };
  });
  await t.check('concept opens', result.heading === 'Concept');
  await t.check('hostile inline script blocked', result.executed === null);
  await t.check('sandbox denies AAC cookies', result.cookieDenied);
  await t.check('CSP denies credentialed API fetch', result.fetchDenied);
  await t.goto('/');
  const hits = await t.page.evaluate(async () => (await (await fetch('/hits')).json()).hits);
  await t.check('no script, image or frame reached protected API', hits === 0);
}
