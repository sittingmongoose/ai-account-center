export default async function (t) {
  await t.goto('/');
  const signedIn = await t.page.evaluate(async () => {
    const cred = { username: 'labadmin', password: 'Lab-Correct-Horse-Battery-9' };
    const check = await (await fetch('/api/auth/check', { credentials: 'same-origin' })).json();
    if (check.authenticated) return { ok: true, status: 200, body: 'already signed in' };
    const setup = await (await fetch('/api/auth/setup', { credentials: 'same-origin' })).json();
    const res = await fetch(setup.configured ? '/api/auth/login' : '/api/auth/setup', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(cred),
    });
    return { ok: res.ok, status: res.status, body: (await res.text()).slice(0, 200) };
  });
  await t.check('signed in', signedIn.ok, `setup/login answered ${signedIn.status}: ${signedIn.body}`, {
    hard: true,
  });
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
